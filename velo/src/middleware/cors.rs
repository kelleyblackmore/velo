//! Cross-origin resource sharing.

use super::{Middleware, Next};
use crate::body::ResBody;
use crate::request::Request;
use crate::response::Response;
use crate::route::BoxFuture;
use http::{header, HeaderName, HeaderValue, Method, StatusCode};
use std::sync::Arc;
use std::time::Duration;

/// Which origins a [`Cors`] layer accepts.
#[derive(Clone, Debug)]
pub enum CorsOrigins {
    /// Reflect any origin. Incompatible with credentials, and rejected at
    /// construction time if you ask for both.
    Any,
    /// An explicit allow-list, compared case-insensitively.
    List(Vec<String>),
}

/// A CORS layer.
///
/// The defaults are the permissive-but-safe ones: any origin, no credentials.
/// Asking for credentials with `Any` origins is a configuration error and
/// panics at build time rather than silently emitting headers browsers ignore.
#[derive(Debug)]
pub struct Cors {
    origins: CorsOrigins,
    methods: Vec<Method>,
    allowed_headers: Vec<HeaderName>,
    exposed_headers: Vec<HeaderName>,
    credentials: bool,
    max_age: Option<Duration>,
}

impl Cors {
    /// Any origin, the common methods, no credentials.
    pub fn permissive() -> Self {
        Self {
            origins: CorsOrigins::Any,
            methods: vec![
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::HEAD,
                Method::OPTIONS,
            ],
            allowed_headers: vec![header::CONTENT_TYPE, header::AUTHORIZATION],
            exposed_headers: Vec::new(),
            credentials: false,
            max_age: Some(Duration::from_secs(3600)),
        }
    }

    /// Restricts to an explicit origin list.
    pub fn allow_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.origins = CorsOrigins::List(origins.into_iter().map(Into::into).collect());
        self
    }

    pub fn allow_methods<I: IntoIterator<Item = Method>>(mut self, methods: I) -> Self {
        self.methods = methods.into_iter().collect();
        self
    }

    pub fn allow_headers<I: IntoIterator<Item = HeaderName>>(mut self, headers: I) -> Self {
        self.allowed_headers = headers.into_iter().collect();
        self
    }

    pub fn expose_headers<I: IntoIterator<Item = HeaderName>>(mut self, headers: I) -> Self {
        self.exposed_headers = headers.into_iter().collect();
        self
    }

    /// Allows cookies and `Authorization` to be sent cross-origin.
    ///
    /// # Panics
    ///
    /// If the origin list is [`CorsOrigins::Any`]. The combination is
    /// forbidden by the CORS specification, and a browser would drop the
    /// response — failing loudly here saves a long debugging session.
    pub fn allow_credentials(mut self, allow: bool) -> Self {
        assert!(
            !(allow && matches!(self.origins, CorsOrigins::Any)),
            "CORS credentials cannot be combined with a wildcard origin; \
             call `allow_origins` with an explicit list first"
        );
        self.credentials = allow;
        self
    }

    pub fn max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    /// The value to send back for a given request origin, if it is allowed.
    fn allowed_origin(&self, origin: &str) -> Option<HeaderValue> {
        match &self.origins {
            CorsOrigins::Any if self.credentials => HeaderValue::from_str(origin).ok(),
            CorsOrigins::Any => Some(HeaderValue::from_static("*")),
            CorsOrigins::List(list) => list
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(origin))
                .then(|| HeaderValue::from_str(origin).ok())
                .flatten(),
        }
    }

    fn join<T: AsRef<str>>(values: &[T]) -> Option<HeaderValue> {
        if values.is_empty() {
            return None;
        }
        let joined = values
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join(", ");
        HeaderValue::from_str(&joined).ok()
    }

    fn apply(&self, headers: &mut http::HeaderMap, origin: HeaderValue) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        if self.credentials {
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
        }
        if matches!(self.origins, CorsOrigins::List(_)) || self.credentials {
            // The response varies by origin, so caches must not share it.
            headers.append(header::VARY, HeaderValue::from_static("Origin"));
        }
        let exposed: Vec<&str> = self.exposed_headers.iter().map(|h| h.as_str()).collect();
        if let Some(value) = Self::join(&exposed) {
            headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, value);
        }
    }
}

impl Default for Cors {
    fn default() -> Self {
        Self::permissive()
    }
}

impl Middleware for Cors {
    fn handle(self: Arc<Self>, req: Request, next: Next) -> BoxFuture<'static, Response> {
        Box::pin(async move {
            let origin = req.header(header::ORIGIN).map(str::to_owned);

            // Not a cross-origin request at all: stay out of the way.
            let Some(origin) = origin else {
                return next.run(req).await;
            };

            let Some(allowed) = self.allowed_origin(&origin) else {
                // An origin we do not allow simply gets no CORS headers; the
                // browser enforces the rest. Returning 403 here would break
                // non-browser clients that send an Origin header.
                return if req.method() == Method::OPTIONS {
                    let mut response = Response::new(ResBody::Empty);
                    *response.status_mut() = StatusCode::FORBIDDEN;
                    response
                } else {
                    next.run(req).await
                };
            };

            if req.method() == Method::OPTIONS
                && req.header(header::ACCESS_CONTROL_REQUEST_METHOD).is_some()
            {
                let mut response = Response::new(ResBody::Empty);
                *response.status_mut() = StatusCode::NO_CONTENT;
                let headers = response.headers_mut();
                self.apply(headers, allowed);

                let methods: Vec<&str> = self.methods.iter().map(Method::as_str).collect();
                if let Some(value) = Cors::join(&methods) {
                    headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, value);
                }
                let allowed_headers: Vec<&str> =
                    self.allowed_headers.iter().map(|h| h.as_str()).collect();
                if let Some(value) = Cors::join(&allowed_headers) {
                    headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
                }
                if let Some(max_age) = self.max_age {
                    if let Ok(value) = HeaderValue::from_str(&max_age.as_secs().to_string()) {
                        headers.insert(header::ACCESS_CONTROL_MAX_AGE, value);
                    }
                }
                return response;
            }

            let mut response = next.run(req).await;
            self.apply(response.headers_mut(), allowed);
            response
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::compose;
    use crate::route::HandlerFn;
    use crate::testing::test_request;

    fn ok_handler() -> HandlerFn {
        Arc::new(|_req| Box::pin(async { Response::new(ResBody::full("ok")) }))
    }

    async fn run(cors: Cors, req: Request) -> Response {
        compose(ok_handler(), &[Arc::new(cors)])(req).await
    }

    #[tokio::test]
    async fn requests_without_an_origin_are_left_alone() {
        let response = run(Cors::permissive(), test_request().build()).await;
        assert!(!response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[tokio::test]
    async fn a_permissive_layer_answers_with_a_wildcard() {
        let response = run(
            Cors::permissive(),
            test_request()
                .header("origin", "https://app.example")
                .build(),
        )
        .await;
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    }

    #[tokio::test]
    async fn a_preflight_advertises_methods_and_headers() {
        let req = test_request()
            .method("OPTIONS")
            .header("origin", "https://app.example")
            .header("access-control-request-method", "POST")
            .build();
        let response = run(Cors::permissive(), req).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let allow_methods = response.headers()[header::ACCESS_CONTROL_ALLOW_METHODS]
            .to_str()
            .unwrap();
        assert!(allow_methods.contains("POST"));
        assert_eq!(response.headers()[header::ACCESS_CONTROL_MAX_AGE], "3600");
    }

    #[tokio::test]
    async fn an_allow_list_reflects_only_listed_origins() {
        let cors = || Cors::permissive().allow_origins(["https://app.example"]);

        let allowed = run(
            cors(),
            test_request()
                .header("origin", "https://app.example")
                .build(),
        )
        .await;
        assert_eq!(
            allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://app.example"
        );
        assert_eq!(allowed.headers()[header::VARY], "Origin");

        let denied = run(
            cors(),
            test_request()
                .header("origin", "https://evil.example")
                .build(),
        )
        .await;
        assert!(!denied
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        // The handler still ran: CORS is enforced by the browser, and a
        // non-browser client should not be blocked by an Origin header.
        assert_eq!(denied.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn credentials_reflect_the_origin_rather_than_a_wildcard() {
        let cors = Cors::permissive()
            .allow_origins(["https://app.example"])
            .allow_credentials(true);
        let response = run(
            cors,
            test_request()
                .header("origin", "https://app.example")
                .build(),
        )
        .await;
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://app.example"
        );
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_ALLOW_CREDENTIALS],
            "true"
        );
    }

    #[test]
    #[should_panic(expected = "wildcard origin")]
    fn credentials_with_a_wildcard_origin_is_rejected_at_build_time() {
        let _ = Cors::permissive().allow_credentials(true);
    }
}
