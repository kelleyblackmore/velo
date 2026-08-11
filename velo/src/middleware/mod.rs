//! Middleware: async layers wrapped around handlers.

mod common;
mod cors;

pub use common::{CatchPanic, Logger, RequestId, RequestIdValue, Timeout};
pub use cors::{Cors, CorsOrigins};

use crate::request::Request;
use crate::response::Response;
use crate::route::{BoxFuture, HandlerFn};
use std::sync::Arc;

/// The rest of the chain.
///
/// Calling [`Next::run`] proceeds inward; not calling it short-circuits, which
/// is how auth and rate-limit layers reject a request without the handler ever
/// seeing it.
pub struct Next(HandlerFn);

impl Next {
    pub(crate) fn new(handler: HandlerFn) -> Self {
        Next(handler)
    }

    /// Runs the remaining chain.
    pub fn run(self, req: Request) -> BoxFuture<'static, Response> {
        (self.0)(req)
    }
}

impl std::fmt::Debug for Next {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Next")
    }
}

/// A layer wrapped around a handler.
///
/// The `Arc<Self>` receiver means a layer can move its own configuration into
/// the returned future without cloning the config on every request.
pub trait Middleware: Send + Sync + 'static {
    fn handle(self: Arc<Self>, req: Request, next: Next) -> BoxFuture<'static, Response>;
}

/// Wraps `handler` in `middleware`, with the first element outermost.
pub(crate) fn compose(handler: HandlerFn, middleware: &[Arc<dyn Middleware>]) -> HandlerFn {
    let mut composed = handler;
    // Fold from the inside out so index 0 ends up seeing the request first.
    for layer in middleware.iter().rev() {
        let layer = Arc::clone(layer);
        let inner = composed;
        composed =
            Arc::new(move |req| Arc::clone(&layer).handle(req, Next::new(Arc::clone(&inner))));
    }
    composed
}

/// Builds a [`Middleware`] from an async closure.
///
/// ```ignore
/// App::new().layer(middleware::from_fn(|req, next| async move {
///     let started = Instant::now();
///     let response = next.run(req).await;
///     tracing::info!(elapsed = ?started.elapsed());
///     response
/// }))
/// ```
pub fn from_fn<F, Fut>(f: F) -> Arc<dyn Middleware>
where
    F: Fn(Request, Next) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send + 'static,
{
    struct FromFn<F>(F);

    impl<F, Fut> Middleware for FromFn<F>
    where
        F: Fn(Request, Next) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Response> + Send + 'static,
    {
        fn handle(self: Arc<Self>, req: Request, next: Next) -> BoxFuture<'static, Response> {
            Box::pin(async move { (self.0)(req, next).await })
        }
    }

    Arc::new(FromFn(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::ResBody;
    use crate::testing::test_request;
    use std::sync::Mutex;

    fn terminal(marker: &'static str) -> HandlerFn {
        Arc::new(move |_req| {
            Box::pin(async move {
                let mut response = Response::new(ResBody::full(marker));
                response
                    .headers_mut()
                    .insert("x-handler", marker.parse().unwrap());
                response
            })
        })
    }

    fn recorder(log: Arc<Mutex<Vec<&'static str>>>, tag: &'static str) -> Arc<dyn Middleware> {
        from_fn(move |req, next| {
            let log = Arc::clone(&log);
            async move {
                log.lock().unwrap().push(tag);
                let response = next.run(req).await;
                log.lock().unwrap().push(tag);
                response
            }
        })
    }

    #[tokio::test]
    async fn layers_run_outermost_first_and_unwind_in_reverse() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let composed = compose(
            terminal("core"),
            &[
                recorder(Arc::clone(&log), "outer"),
                recorder(Arc::clone(&log), "inner"),
            ],
        );

        composed(test_request().build()).await;
        assert_eq!(
            *log.lock().unwrap(),
            vec!["outer", "inner", "inner", "outer"]
        );
    }

    #[tokio::test]
    async fn a_layer_can_short_circuit_without_calling_next() {
        use crate::response::IntoResponse;

        // The inner handler sets this header; a short-circuit means it never
        // runs, so the header must be absent.
        let gate =
            from_fn(|_req, _next| async move { http::StatusCode::FORBIDDEN.into_response() });

        let composed = compose(terminal("core"), &[gate]);
        let response = composed(test_request().build()).await;
        assert_eq!(response.status(), http::StatusCode::FORBIDDEN);
        assert!(!response.headers().contains_key("x-handler"));
    }

    #[tokio::test]
    async fn no_layers_means_the_handler_runs_untouched() {
        let composed = compose(terminal("core"), &[]);
        let response = composed(test_request().build()).await;
        assert_eq!(response.headers()["x-handler"], "core");
    }
}
