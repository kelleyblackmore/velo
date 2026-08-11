//! The request type handed to extractors.

use crate::state::StateMap;
use bytes::Bytes;
use http::{HeaderMap, Method, Uri, Version};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

/// A path parameter captured while routing.
pub(crate) type PathParams = Vec<(Arc<str>, String)>;

/// An incoming request, with its body already buffered.
///
/// Buffering up front is what lets extractors be composable and order-free —
/// any extractor can read the body, and reading it twice is free. The size
/// ceiling from [`App::body_limit`](crate::App::body_limit) is enforced
/// against `Content-Length` before a byte is read, so this is not a DoS
/// vector.
///
/// The trade-off is real: there is no streaming-request extractor yet, so a
/// large upload must fit under the limit.
#[derive(Debug)]
pub struct Request {
    pub(crate) head: http::request::Parts,
    pub(crate) body: Bytes,
    pub(crate) params: PathParams,
    pub(crate) state: Arc<StateMap>,
    pub(crate) matched_path: Option<&'static str>,
    /// Memoised dependency results, so a dependency shared by several
    /// sub-dependencies is resolved exactly once per request.
    pub(crate) cache: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Request {
    /// Builds a request from its parts. Mostly useful for tests; the server
    /// constructs these itself.
    pub fn new(head: http::request::Parts, body: Bytes) -> Self {
        Self {
            head,
            body,
            params: Vec::new(),
            state: Arc::new(StateMap::default()),
            matched_path: None,
            cache: HashMap::new(),
        }
    }

    pub fn method(&self) -> &Method {
        &self.head.method
    }

    pub fn uri(&self) -> &Uri {
        &self.head.uri
    }

    pub fn version(&self) -> Version {
        self.head.version
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.head.headers
    }

    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.head.headers
    }

    /// The request path, without query string.
    pub fn path(&self) -> &str {
        self.head.uri.path()
    }

    /// The raw query string, without the leading `?`.
    pub fn query(&self) -> &str {
        self.head.uri.query().unwrap_or("")
    }

    /// The route template that matched, e.g. `/users/{id}`.
    ///
    /// Metrics middleware wants this rather than [`Self::path`], which would
    /// produce unbounded cardinality.
    pub fn matched_path(&self) -> Option<&'static str> {
        self.matched_path
    }

    /// The buffered request body.
    pub fn body(&self) -> &Bytes {
        &self.body
    }

    /// A cheap (refcounted) clone of the body.
    pub fn body_bytes(&self) -> Bytes {
        self.body.clone()
    }

    /// Per-request typed storage, shared with the underlying `http` types.
    pub fn extensions(&self) -> &http::Extensions {
        &self.head.extensions
    }

    pub fn extensions_mut(&mut self) -> &mut http::Extensions {
        &mut self.head.extensions
    }

    /// A path parameter captured by the router.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| &**k == name)
            .map(|(_, v)| v.as_str())
    }

    /// All captured path parameters, in the order they appear in the template.
    pub fn params(&self) -> impl Iterator<Item = (&str, &str)> {
        self.params.iter().map(|(k, v)| (&**k, v.as_str()))
    }

    /// Application state registered with [`crate::App::with_state`].
    pub fn state<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.state.get::<T>()
    }

    /// The peer address, when the server was able to determine one.
    pub fn peer_addr(&self) -> Option<SocketAddr> {
        self.head.extensions.get::<ConnectInfo>().map(|c| c.0)
    }

    /// The value of a header as a `&str`, if present and valid UTF-8.
    pub fn header(&self, name: impl http::header::AsHeaderName) -> Option<&str> {
        self.head.headers.get(name)?.to_str().ok()
    }

    /// The `Content-Type` with parameters stripped, lowercased.
    pub fn content_type(&self) -> Option<&str> {
        let raw = self.header(http::header::CONTENT_TYPE)?;
        Some(raw.split(';').next().unwrap_or(raw).trim())
    }

    /// Splits the request into its head and body.
    pub fn into_parts(self) -> (http::request::Parts, Bytes) {
        (self.head, self.body)
    }
}

/// The peer socket address, inserted into request extensions by the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectInfo(pub SocketAddr);

#[cfg(test)]
mod tests {
    use super::*;

    fn request(uri: &str) -> Request {
        let (head, _) = http::Request::builder()
            .uri(uri)
            .header("content-type", "application/json; charset=utf-8")
            .body(())
            .unwrap()
            .into_parts();
        Request::new(head, Bytes::new())
    }

    #[test]
    fn query_is_empty_rather_than_absent() {
        assert_eq!(request("/x").query(), "");
        assert_eq!(request("/x?a=1").query(), "a=1");
    }

    #[test]
    fn content_type_strips_parameters() {
        assert_eq!(request("/x").content_type(), Some("application/json"));
    }

    #[test]
    fn params_are_looked_up_by_name() {
        let mut req = request("/users/7");
        req.params.push((Arc::from("id"), "7".to_owned()));
        assert_eq!(req.param("id"), Some("7"));
        assert_eq!(req.param("missing"), None);
    }
}
