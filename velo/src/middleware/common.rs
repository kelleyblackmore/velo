//! The layers almost every service ends up wanting.

use super::{Middleware, Next};
use crate::error::ApiError;
use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::route::BoxFuture;
use http::{HeaderName, HeaderValue, StatusCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The correlation id for a request, available via
/// `Extension<RequestIdValue>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestIdValue(pub String);

impl std::fmt::Display for RequestIdValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Propagates an inbound `x-request-id`, or mints one, and echoes it on the
/// response.
///
/// Trusting a client-supplied id is the default because it is what makes
/// tracing across services work; [`RequestId::always_generate`] turns that off
/// for edge-facing servers where the header is attacker-controlled.
#[derive(Debug)]
pub struct RequestId {
    header: HeaderName,
    trust_inbound: bool,
}

impl RequestId {
    pub fn new() -> Self {
        Self {
            header: HeaderName::from_static("x-request-id"),
            trust_inbound: true,
        }
    }

    /// Ignores any inbound value and always mints a fresh id.
    pub fn always_generate(mut self) -> Self {
        self.trust_inbound = false;
        self
    }

    /// Uses a different header name.
    pub fn header(mut self, name: HeaderName) -> Self {
        self.header = name;
        self
    }

    /// A short, collision-resistant-enough id: process entropy plus a counter.
    fn generate() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

        let seed = *SEED.get_or_init(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15)
        });
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{seed:016x}{n:08x}")
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for RequestId {
    fn handle(self: Arc<Self>, mut req: Request, next: Next) -> BoxFuture<'static, Response> {
        Box::pin(async move {
            let id = self
                .trust_inbound
                .then(|| req.header(&self.header).map(str::to_owned))
                .flatten()
                .filter(|value| {
                    // A header echoed into a response must be a legal header
                    // value, and an unbounded id is a log-injection risk.
                    !value.is_empty()
                        && value.len() <= 128
                        && value.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
                })
                .unwrap_or_else(RequestId::generate);

            req.extensions_mut().insert(RequestIdValue(id.clone()));
            let mut response = next.run(req).await;

            if let Ok(value) = HeaderValue::from_str(&id) {
                response.headers_mut().insert(self.header.clone(), value);
            }
            response
        })
    }
}

/// Fails a request that outruns its budget with `504 Gateway Timeout`.
#[derive(Debug)]
pub struct Timeout {
    budget: Duration,
}

impl Timeout {
    pub fn new(budget: Duration) -> Self {
        Self { budget }
    }

    pub fn seconds(seconds: u64) -> Self {
        Self::new(Duration::from_secs(seconds))
    }
}

impl Middleware for Timeout {
    fn handle(self: Arc<Self>, req: Request, next: Next) -> BoxFuture<'static, Response> {
        Box::pin(async move {
            match tokio::time::timeout(self.budget, next.run(req)).await {
                Ok(response) => response,
                Err(_) => ApiError::new(StatusCode::GATEWAY_TIMEOUT)
                    .with_detail(format!(
                        "The request exceeded its {:?} budget.",
                        self.budget
                    ))
                    .into_response(),
            }
        })
    }
}

/// Turns a panicking handler into a `500` instead of a dropped connection.
///
/// A panic in one request should not take down the connection — or, with
/// `panic = "abort"`, the process. This layer is mounted by default.
#[derive(Debug, Default)]
pub struct CatchPanic;

impl Middleware for CatchPanic {
    fn handle(self: Arc<Self>, req: Request, next: Next) -> BoxFuture<'static, Response> {
        use futures_util::FutureExt;
        Box::pin(async move {
            let matched = req.matched_path().unwrap_or("<unmatched>");
            match std::panic::AssertUnwindSafe(next.run(req))
                .catch_unwind()
                .await
            {
                Ok(response) => response,
                Err(panic) => {
                    let message = panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_owned())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "handler panicked".to_owned());
                    ApiError::internal(format!("panic in `{matched}`: {message}")).into_response()
                }
            }
        })
    }
}

/// Records the outcome of every request.
///
/// With the `tracing` feature this emits `tracing` events; otherwise it calls
/// the closure you supply, so the layer is useful without pulling in a
/// logging stack.
pub struct Logger {
    sink: Box<dyn Fn(RequestLog<'_>) + Send + Sync>,
}

/// What [`Logger`] reports.
#[derive(Debug)]
pub struct RequestLog<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// The route template, which is the safe thing to use as a metric label.
    pub matched_path: Option<&'static str>,
    pub status: u16,
    pub elapsed: Duration,
    pub request_id: Option<&'a str>,
}

impl Logger {
    /// Logs through `tracing` when the feature is on, and to stderr otherwise.
    pub fn new() -> Self {
        Self::with(|log| {
            #[cfg(feature = "tracing")]
            {
                if log.status >= 500 {
                    tracing::error!(
                        method = log.method,
                        path = log.path,
                        status = log.status,
                        elapsed_ms = log.elapsed.as_millis() as u64,
                        request_id = log.request_id,
                        "request failed"
                    );
                } else {
                    tracing::info!(
                        method = log.method,
                        path = log.path,
                        status = log.status,
                        elapsed_ms = log.elapsed.as_millis() as u64,
                        request_id = log.request_id,
                        "request"
                    );
                }
            }
            #[cfg(not(feature = "tracing"))]
            {
                eprintln!(
                    "{} {} -> {} ({:?})",
                    log.method, log.path, log.status, log.elapsed
                );
            }
        })
    }

    /// Sends each record to a closure of your choosing.
    pub fn with<F: Fn(RequestLog<'_>) + Send + Sync + 'static>(sink: F) -> Self {
        Self {
            sink: Box::new(sink),
        }
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Logger")
    }
}

impl Middleware for Logger {
    fn handle(self: Arc<Self>, req: Request, next: Next) -> BoxFuture<'static, Response> {
        Box::pin(async move {
            let started = Instant::now();
            let method = req.method().as_str().to_owned();
            let path = req.path().to_owned();
            let matched_path = req.matched_path();
            let request_id = req
                .extensions()
                .get::<RequestIdValue>()
                .map(|id| id.0.clone());

            let response = next.run(req).await;

            (self.sink)(RequestLog {
                method: &method,
                path: &path,
                matched_path,
                status: response.status().as_u16(),
                elapsed: started.elapsed(),
                request_id: request_id.as_deref(),
            });

            response
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::ResBody;
    use crate::middleware::compose;
    use crate::route::HandlerFn;
    use crate::testing::test_request;
    use std::sync::Mutex;

    fn ok_handler() -> HandlerFn {
        Arc::new(|_req| Box::pin(async { Response::new(ResBody::full("ok")) }))
    }

    #[tokio::test]
    async fn request_id_is_generated_and_echoed() {
        let composed = compose(ok_handler(), &[Arc::new(RequestId::new())]);
        let response = composed(test_request().build()).await;
        let id = response.headers()["x-request-id"].to_str().unwrap();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn an_inbound_request_id_is_propagated() {
        let composed = compose(ok_handler(), &[Arc::new(RequestId::new())]);
        let response = composed(
            test_request()
                .header("x-request-id", "trace-abc-123")
                .build(),
        )
        .await;
        assert_eq!(response.headers()["x-request-id"], "trace-abc-123");
    }

    #[tokio::test]
    async fn a_hostile_inbound_id_is_replaced_not_echoed() {
        let composed = compose(ok_handler(), &[Arc::new(RequestId::new())]);
        let response = composed(
            test_request()
                .header("x-request-id", &"x".repeat(500))
                .build(),
        )
        .await;
        assert_ne!(response.headers()["x-request-id"].len(), 500);
    }

    #[tokio::test]
    async fn always_generate_ignores_the_client() {
        let composed = compose(
            ok_handler(),
            &[Arc::new(RequestId::new().always_generate())],
        );
        let response = composed(test_request().header("x-request-id", "spoofed").build()).await;
        assert_ne!(response.headers()["x-request-id"], "spoofed");
    }

    #[tokio::test]
    async fn generated_ids_are_unique() {
        let a = RequestId::generate();
        let b = RequestId::generate();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn a_slow_handler_times_out_with_504() {
        let slow: HandlerFn = Arc::new(|_req| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Response::new(ResBody::Empty)
            })
        });
        let composed = compose(slow, &[Arc::new(Timeout::new(Duration::from_millis(10)))]);
        let response = composed(test_request().build()).await;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn a_fast_handler_is_untouched_by_the_timeout() {
        let composed = compose(ok_handler(), &[Arc::new(Timeout::seconds(30))]);
        assert_eq!(composed(test_request().build()).await.status(), 200);
    }

    #[tokio::test]
    async fn a_panicking_handler_becomes_a_500() {
        let boom: HandlerFn = Arc::new(|_req| Box::pin(async { panic!("kaboom") }));
        let composed = compose(boom, &[Arc::new(CatchPanic)]);
        let response = composed(test_request().build()).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn the_logger_reports_method_path_and_status() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let logger = Logger::with(move |log| {
            sink.lock()
                .unwrap()
                .push((log.method.to_owned(), log.path.to_owned(), log.status));
        });

        let composed = compose(ok_handler(), &[Arc::new(logger)]);
        composed(test_request().uri("/things?x=1").build()).await;

        assert_eq!(
            *seen.lock().unwrap(),
            vec![("GET".to_owned(), "/things".to_owned(), 200)]
        );
    }
}
