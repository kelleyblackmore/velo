//! Testing utilities.
//!
//! Requests are dispatched through the same `Service::handle` the server uses,
//! so a test exercises real routing, real extractors, and real middleware —
//! there is no parallel "test mode" code path to drift out of sync.

use crate::app::{App, Service};
use crate::request::Request;
use crate::response::Response;
use crate::state::StateMap;
use bytes::Bytes;
use http::{HeaderName, HeaderValue, StatusCode};
use std::sync::Arc;

/// Starts building a [`Request`] for unit-testing an extractor.
pub fn test_request() -> TestRequest {
    TestRequest::new()
}

/// A request builder.
#[derive(Debug)]
pub struct TestRequest {
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Bytes,
    params: Vec<(String, String)>,
    state: Option<Arc<StateMap>>,
    matched_path: Option<&'static str>,
}

impl Default for TestRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRequest {
    pub fn new() -> Self {
        Self {
            method: "GET".into(),
            uri: "/".into(),
            headers: Vec::new(),
            body: Bytes::new(),
            params: Vec::new(),
            state: None,
            matched_path: None,
        }
    }

    pub fn method(mut self, method: &str) -> Self {
        self.method = method.to_ascii_uppercase();
        self
    }

    pub fn uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = uri.into();
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// Sets a JSON body and the matching `Content-Type`.
    pub fn json(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self.headers
            .push(("content-type".into(), "application/json".into()));
        self
    }

    /// Sets a form body and the matching `Content-Type`.
    pub fn form(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self.headers.push((
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        self
    }

    /// Pre-populates a path parameter, as the router would.
    pub fn param(mut self, name: &str, value: &str) -> Self {
        self.params.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn state(mut self, state: Arc<StateMap>) -> Self {
        self.state = Some(state);
        self
    }

    pub fn matched_path(mut self, path: &'static str) -> Self {
        self.matched_path = Some(path);
        self
    }

    /// Builds the request.
    ///
    /// # Panics
    ///
    /// If the method, URI, or headers are not valid HTTP syntax. This is test
    /// support, so a typo should fail immediately and loudly.
    pub fn build(self) -> Request {
        let mut builder = http::Request::builder()
            .method(self.method.as_str())
            .uri(&self.uri);

        for (name, value) in &self.headers {
            let name = HeaderName::try_from(name.as_str())
                .unwrap_or_else(|_| panic!("`{name}` is not a valid header name"));
            let value = HeaderValue::from_str(value)
                .unwrap_or_else(|_| panic!("`{value}` is not a valid header value"));
            builder = builder.header(name, value);
        }

        let (head, _) = builder
            .body(())
            .expect("test request should be well-formed")
            .into_parts();

        let mut request = Request::new(head, self.body);
        request.params = self
            .params
            .into_iter()
            .map(|(k, v)| (Arc::from(k.as_str()), v))
            .collect();
        request.matched_path = self.matched_path;
        if let Some(state) = self.state {
            request.state = state;
        }
        request
    }
}

/// An in-process client that drives a built [`Service`].
#[derive(Debug)]
pub struct TestClient {
    service: Service,
}

impl TestClient {
    /// Builds an app into a client.
    ///
    /// # Panics
    ///
    /// If the routes conflict — a wiring bug that should fail the test.
    pub fn new(app: App) -> Self {
        Self {
            service: app.build().expect("routes should not conflict"),
        }
    }

    /// The document the service serves.
    pub fn openapi(&self) -> &velo_openapi::OpenApi {
        self.service.openapi()
    }

    /// The underlying service.
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// Sends a request and reads the whole response.
    pub async fn send(&self, request: TestRequest) -> TestResponse {
        let TestRequest {
            method,
            uri,
            headers,
            body,
            ..
        } = request;

        let mut builder = http::Request::builder().method(method.as_str()).uri(&uri);
        for (name, value) in &headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let (head, _) = builder
            .body(())
            .expect("test request should be well-formed")
            .into_parts();

        TestResponse::collect(self.service.handle(head, body).await).await
    }

    pub async fn get(&self, uri: &str) -> TestResponse {
        self.send(test_request().uri(uri)).await
    }

    pub async fn post_json(&self, uri: &str, body: &str) -> TestResponse {
        self.send(test_request().method("POST").uri(uri).json(body.to_owned()))
            .await
    }

    pub async fn delete(&self, uri: &str) -> TestResponse {
        self.send(test_request().method("DELETE").uri(uri)).await
    }
}

/// A response with its body already read.
#[derive(Debug)]
pub struct TestResponse {
    status: StatusCode,
    headers: http::HeaderMap,
    body: Bytes,
}

impl TestResponse {
    async fn collect(response: Response) -> Self {
        use http_body_util::BodyExt;
        let (parts, body) = response.into_parts();
        let body = body
            .collect()
            .await
            .map(|collected| collected.to_bytes())
            .unwrap_or_default();
        Self {
            status: parts.status,
            headers: parts.headers,
            body,
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }

    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// The body as text.
    ///
    /// # Panics
    ///
    /// If the body is not valid UTF-8.
    pub fn text(&self) -> String {
        String::from_utf8(self.body.to_vec()).expect("response body should be UTF-8")
    }

    /// The body parsed as JSON.
    ///
    /// # Panics
    ///
    /// If the body is not valid JSON, with the body included in the message so
    /// a failing test says what actually came back.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "response body is not JSON ({error}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// Deserialises the body into `T`.
    pub fn json_as<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "response body does not match `{}` ({error}): {}",
                std::any::type_name::<T>(),
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// Asserts the status, showing the body when it does not match.
    ///
    /// # Panics
    ///
    /// If the status differs.
    pub fn assert_status(&self, expected: StatusCode) -> &Self {
        assert_eq!(
            self.status,
            expected,
            "expected {expected}, got {}: {}",
            self.status,
            String::from_utf8_lossy(&self.body)
        );
        self
    }
}
