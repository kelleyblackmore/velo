//! Path matching.
//!
//! A segment trie with backtracking: static segments beat parameters, and
//! parameters beat catch-alls, at every level rather than only at the first
//! divergence. That ordering is what makes `/files/latest` reachable when
//! `/files/{*path}` is also registered.

use crate::middleware::Middleware;
use crate::request::PathParams;
use crate::route::{HandlerFn, IntoRoute, RouteDef};
use std::collections::HashMap;
use std::sync::Arc;

/// A mounted endpoint: the fully composed handler plus its template.
pub struct Endpoint {
    pub handler: HandlerFn,
    pub matched_path: &'static str,
    pub method: &'static str,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("method", &self.method)
            .field("matched_path", &self.matched_path)
            .finish_non_exhaustive()
    }
}

/// The outcome of matching a request line.
#[derive(Debug)]
pub enum Match {
    /// A handler was found.
    Found {
        endpoint: Arc<Endpoint>,
        params: PathParams,
    },
    /// The path exists but not for this method; carries the `Allow` list.
    MethodNotAllowed(Vec<&'static str>),
    /// Nothing matched.
    NotFound,
}

#[derive(Default)]
struct Node {
    statics: HashMap<String, Node>,
    /// `{name}` — matches exactly one segment.
    param: Option<Box<ParamNode>>,
    /// `{*name}` — matches the remainder of the path.
    wildcard: Option<Box<WildcardNode>>,
    methods: HashMap<&'static str, Arc<Endpoint>>,
}

struct ParamNode {
    name: Arc<str>,
    node: Node,
}

struct WildcardNode {
    name: Arc<str>,
    methods: HashMap<&'static str, Arc<Endpoint>>,
}

/// The compiled routing table.
#[derive(Default)]
pub struct RouteTable {
    root: Node,
    /// Every registered (method, template) pair, for diagnostics and docs.
    registered: Vec<(&'static str, &'static str)>,
}

impl std::fmt::Debug for RouteTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteTable")
            .field("routes", &self.registered)
            .finish()
    }
}

/// Splits a path into non-empty segments, so `/a/b`, `/a/b/`, and `//a//b`
/// all route identically. Trailing-slash-only differences are a common source
/// of accidental 404s and are not worth preserving.
fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// A template segment.
enum Segment<'a> {
    Static(&'a str),
    Param(&'a str),
    Wildcard(&'a str),
}

fn parse_segment(raw: &str) -> Segment<'_> {
    match raw.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        Some(inner) => match inner.strip_prefix('*') {
            Some(name) => Segment::Wildcard(name),
            None => Segment::Param(inner),
        },
        None => Segment::Static(raw),
    }
}

/// Raised when two routes cannot coexist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteConflict {
    /// The same method and template registered twice.
    Duplicate {
        method: &'static str,
        path: &'static str,
    },
    /// Two different parameter names at the same position, e.g. `/a/{id}` and
    /// `/a/{name}`. Allowing this would make the captured name depend on
    /// registration order.
    ParameterName {
        path: &'static str,
        existing: String,
        incoming: String,
    },
    /// A catch-all that is not the final segment.
    TrailingWildcard { path: &'static str },
}

impl std::fmt::Display for RouteConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteConflict::Duplicate { method, path } => {
                write!(f, "`{method} {path}` is registered more than once")
            }
            RouteConflict::ParameterName {
                path,
                existing,
                incoming,
            } => write!(
                f,
                "`{path}` names a path parameter `{incoming}` where `{existing}` \
                 is already used at that position; pick one name"
            ),
            RouteConflict::TrailingWildcard { path } => {
                write!(f, "`{path}` has a catch-all that is not the last segment")
            }
        }
    }
}

impl std::error::Error for RouteConflict {}

impl RouteTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a route, wrapping its handler in `middleware` (outermost first).
    pub fn insert(
        &mut self,
        route: RouteDef,
        middleware: &[Arc<dyn Middleware>],
    ) -> Result<(), RouteConflict> {
        let RouteDef {
            method,
            path,
            handler,
            ..
        } = route;

        let handler = crate::middleware::compose(handler, middleware);
        let endpoint = Arc::new(Endpoint {
            handler,
            matched_path: path,
            method,
        });

        let mut node = &mut self.root;
        let mut parts = segments(path).peekable();

        while let Some(raw) = parts.next() {
            match parse_segment(raw) {
                Segment::Static(name) => {
                    node = node.statics.entry(name.to_owned()).or_default();
                }
                Segment::Param(name) => {
                    let param = node.param.get_or_insert_with(|| {
                        Box::new(ParamNode {
                            name: Arc::from(name),
                            node: Node::default(),
                        })
                    });
                    if &*param.name != name {
                        return Err(RouteConflict::ParameterName {
                            path,
                            existing: param.name.to_string(),
                            incoming: name.to_owned(),
                        });
                    }
                    node = &mut param.node;
                }
                Segment::Wildcard(name) => {
                    if parts.peek().is_some() {
                        return Err(RouteConflict::TrailingWildcard { path });
                    }
                    let wildcard = node.wildcard.get_or_insert_with(|| {
                        Box::new(WildcardNode {
                            name: Arc::from(name),
                            methods: HashMap::new(),
                        })
                    });
                    if wildcard.methods.contains_key(method) {
                        return Err(RouteConflict::Duplicate { method, path });
                    }
                    wildcard.methods.insert(method, endpoint);
                    self.registered.push((method, path));
                    return Ok(());
                }
            }
        }

        if node.methods.contains_key(method) {
            return Err(RouteConflict::Duplicate { method, path });
        }
        node.methods.insert(method, endpoint);
        self.registered.push((method, path));
        Ok(())
    }

    /// Matches a request path and method.
    pub fn find(&self, path: &str, method: &str) -> Match {
        let parts: Vec<&str> = segments(path).collect();
        let mut params = PathParams::new();

        if let Some(endpoint) = find_endpoint(&self.root, &parts, 0, method, &mut params) {
            return Match::Found { endpoint, params };
        }

        // No handler for this method. Walk again ignoring the method so a
        // known path answers 405 with an accurate `Allow` list instead of
        // pretending the resource does not exist.
        let mut allowed: Vec<&'static str> = Vec::new();
        walk(&self.root, &parts, 0, method, &mut allowed);

        if allowed.is_empty() {
            Match::NotFound
        } else {
            allowed.sort_unstable();
            allowed.dedup();
            Match::MethodNotAllowed(allowed)
        }
    }

    /// Every registered `(method, template)` pair.
    pub fn registered(&self) -> &[(&'static str, &'static str)] {
        &self.registered
    }

    pub fn len(&self) -> usize {
        self.registered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.registered.is_empty()
    }
}

/// Finds a matching endpoint, filling `params` along the way.
fn find_endpoint(
    node: &Node,
    parts: &[&str],
    index: usize,
    method: &str,
    params: &mut PathParams,
) -> Option<Arc<Endpoint>> {
    if index == parts.len() {
        if let Some(endpoint) = node.methods.get(method) {
            return Some(Arc::clone(endpoint));
        }
        // An empty tail can still satisfy a catch-all: `/files/{*path}` should
        // match `/files`.
        if let Some(wildcard) = &node.wildcard {
            if let Some(endpoint) = wildcard.methods.get(method) {
                params.push((Arc::clone(&wildcard.name), String::new()));
                return Some(Arc::clone(endpoint));
            }
        }
        return None;
    }

    let segment = parts[index];

    if let Some(child) = node.statics.get(segment) {
        if let Some(found) = find_endpoint(child, parts, index + 1, method, params) {
            return Some(found);
        }
    }

    if let Some(param) = &node.param {
        let checkpoint = params.len();
        params.push((Arc::clone(&param.name), decode_segment(segment)));
        if let Some(found) = find_endpoint(&param.node, parts, index + 1, method, params) {
            return Some(found);
        }
        // Backtrack: this branch did not lead to a handler.
        params.truncate(checkpoint);
    }

    if let Some(wildcard) = &node.wildcard {
        if let Some(endpoint) = wildcard.methods.get(method) {
            let rest = parts[index..].join("/");
            params.push((Arc::clone(&wildcard.name), decode_segment(&rest)));
            return Some(Arc::clone(endpoint));
        }
    }

    None
}

/// Walks the trie ignoring the method, recording which methods *are* available
/// on any path that matches. Used to answer 405 rather than 404.
fn walk(node: &Node, parts: &[&str], index: usize, method: &str, allowed: &mut Vec<&'static str>) {
    if index == parts.len() {
        allowed.extend(node.methods.keys().filter(|k| **k != method));
        if let Some(wildcard) = &node.wildcard {
            allowed.extend(wildcard.methods.keys().filter(|k| **k != method));
        }
        return;
    }

    if let Some(child) = node.statics.get(parts[index]) {
        walk(child, parts, index + 1, method, allowed);
    }
    if let Some(param) = &node.param {
        walk(&param.node, parts, index + 1, method, allowed);
    }
    if let Some(wildcard) = &node.wildcard {
        allowed.extend(wildcard.methods.keys().filter(|k| **k != method));
    }
}

/// Percent-decodes a captured segment. Matching happens on the raw path so a
/// `%2F` cannot smuggle an extra segment past the trie.
fn decode_segment(segment: &str) -> String {
    if segment.contains('%') {
        percent_encoding::percent_decode_str(segment)
            .decode_utf8_lossy()
            .into_owned()
    } else {
        segment.to_owned()
    }
}

/// Builds a table from routes and middleware.
pub fn build(
    routes: Vec<RouteDef>,
    middleware: &[Arc<dyn Middleware>],
) -> Result<RouteTable, RouteConflict> {
    let mut table = RouteTable::new();
    for route in routes {
        table.insert(route, middleware)?;
    }
    Ok(table)
}

/// Convenience for tests and manual wiring.
pub fn table_from<I: IntoIterator<Item = R>, R: IntoRoute>(
    routes: I,
) -> Result<RouteTable, RouteConflict> {
    build(routes.into_iter().map(IntoRoute::into_route).collect(), &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::ResBody;
    use crate::response::Response;

    fn route(method: &'static str, path: &'static str) -> RouteDef {
        RouteDef::raw(method, path, |_req| async { Response::new(ResBody::Empty) })
    }

    fn table(routes: Vec<RouteDef>) -> RouteTable {
        build(routes, &[]).expect("routes should not conflict")
    }

    fn found(table: &RouteTable, path: &str, method: &str) -> (&'static str, PathParams) {
        match table.find(path, method) {
            Match::Found { endpoint, params } => (endpoint.matched_path, params),
            other => panic!("expected a match for {method} {path}, got {other:?}"),
        }
    }

    #[test]
    fn static_routes_match_exactly() {
        let table = table(vec![route("GET", "/health")]);
        assert_eq!(found(&table, "/health", "GET").0, "/health");
        assert!(matches!(table.find("/healthz", "GET"), Match::NotFound));
    }

    #[test]
    fn parameters_are_captured_by_name() {
        let table = table(vec![route("GET", "/users/{id}")]);
        let (_, params) = found(&table, "/users/42", "GET");
        assert_eq!(params, vec![(Arc::from("id"), "42".to_owned())]);
    }

    #[test]
    fn static_segments_win_over_parameters() {
        let table = table(vec![route("GET", "/users/{id}"), route("GET", "/users/me")]);
        assert_eq!(found(&table, "/users/me", "GET").0, "/users/me");
        assert_eq!(found(&table, "/users/7", "GET").0, "/users/{id}");
    }

    #[test]
    fn matching_backtracks_out_of_a_dead_end() {
        // `/a/{x}/c` and `/a/b/d` diverge only at the third segment, so
        // matching `/a/b/c` must abandon the static `b` branch and retry the
        // parameter branch.
        let table = table(vec![route("GET", "/a/{x}/c"), route("GET", "/a/b/d")]);
        let (path, params) = found(&table, "/a/b/c", "GET");
        assert_eq!(path, "/a/{x}/c");
        assert_eq!(params[0].1, "b");
    }

    #[test]
    fn catch_all_captures_the_remainder() {
        let table = table(vec![route("GET", "/files/{*path}")]);
        let (_, params) = found(&table, "/files/a/b/c.txt", "GET");
        assert_eq!(params[0].1, "a/b/c.txt");
    }

    #[test]
    fn catch_all_yields_to_a_more_specific_route() {
        let table = table(vec![
            route("GET", "/files/{*path}"),
            route("GET", "/files/latest"),
        ]);
        assert_eq!(found(&table, "/files/latest", "GET").0, "/files/latest");
        assert_eq!(found(&table, "/files/x/y", "GET").0, "/files/{*path}");
    }

    #[test]
    fn catch_all_matches_an_empty_tail() {
        let table = table(vec![route("GET", "/files/{*path}")]);
        let (_, params) = found(&table, "/files", "GET");
        assert_eq!(params[0].1, "");
    }

    #[test]
    fn a_known_path_with_an_unknown_method_is_405_with_allow() {
        let table = table(vec![route("GET", "/users"), route("POST", "/users")]);
        match table.find("/users", "DELETE") {
            Match::MethodNotAllowed(allowed) => assert_eq!(allowed, vec!["GET", "POST"]),
            other => panic!("expected 405, got {other:?}"),
        }
    }

    #[test]
    fn trailing_slashes_do_not_change_the_match() {
        let table = table(vec![route("GET", "/users")]);
        assert_eq!(found(&table, "/users/", "GET").0, "/users");
        assert_eq!(found(&table, "//users", "GET").0, "/users");
    }

    #[test]
    fn encoded_separators_cannot_forge_an_extra_segment() {
        let table = table(vec![route("GET", "/files/{name}")]);
        // `%2F` decodes to `/`, but only after matching, so this stays one
        // segment rather than becoming `/files/a/b`.
        let (path, params) = found(&table, "/files/a%2Fb", "GET");
        assert_eq!(path, "/files/{name}");
        assert_eq!(params[0].1, "a/b");
    }

    #[test]
    fn duplicate_registrations_are_rejected() {
        let err = build(vec![route("GET", "/users"), route("GET", "/users")], &[]).unwrap_err();
        assert!(matches!(err, RouteConflict::Duplicate { .. }));
    }

    #[test]
    fn conflicting_parameter_names_are_rejected() {
        let err = build(
            vec![route("GET", "/users/{id}"), route("GET", "/users/{name}")],
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, RouteConflict::ParameterName { .. }));
        assert!(err.to_string().contains("pick one name"));
    }

    #[test]
    fn a_catch_all_must_come_last() {
        let err = build(vec![route("GET", "/files/{*path}/meta")], &[]).unwrap_err();
        assert!(matches!(err, RouteConflict::TrailingWildcard { .. }));
    }

    #[test]
    fn the_same_path_may_carry_several_methods() {
        let table = table(vec![
            route("GET", "/users/{id}"),
            route("PUT", "/users/{id}"),
            route("DELETE", "/users/{id}"),
        ]);
        assert_eq!(table.len(), 3);
        for method in ["GET", "PUT", "DELETE"] {
            assert_eq!(found(&table, "/users/9", method).0, "/users/{id}");
        }
    }

    #[test]
    fn multi_segment_parameters_capture_in_order() {
        let table = table(vec![route("GET", "/orgs/{org}/repos/{repo}")]);
        let (_, params) = found(&table, "/orgs/acme/repos/velo", "GET");
        assert_eq!(params[0], (Arc::from("org"), "acme".to_owned()));
        assert_eq!(params[1], (Arc::from("repo"), "velo".to_owned()));
    }
}
