//! The application builder and the server that runs it.

use crate::body::ResBody;
use crate::docs::Docs;
use crate::error::ApiError;
use crate::extract::BEARER_SCHEME;
use crate::middleware::{CatchPanic, Middleware};
use crate::operation::OperationContext;
use crate::request::{ConnectInfo, Request};
use crate::response::{IntoResponse, Response};
use crate::route::{BoxFuture, IntoRoute, RouteDef};
use crate::router::{Match, RouteConflict, RouteTable};
use crate::state::StateMap;
use bytes::Bytes;
use http::{header, HeaderValue, Method, StatusCode};
use std::net::SocketAddr;
use std::sync::Arc;
use velo_openapi::{
    Components, Info, Map, OpenApi, Operation, PathItem, SchemaGenerator, SecurityScheme, Server,
    Tag,
};

/// The default ceiling on a buffered request body: 2 MiB.
pub const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// A group of routes sharing a prefix, tags, and middleware.
///
/// Nesting composes all three, so an `/admin` router can carry its own auth
/// layer and have every operation inside it tagged accordingly without
/// repeating anything per route.
#[derive(Default)]
pub struct Router {
    routes: Vec<RouteEntry>,
    middleware: Vec<Arc<dyn Middleware>>,
}

struct RouteEntry {
    def: RouteDef,
    tags: Vec<String>,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field(
                "routes",
                &self
                    .routes
                    .iter()
                    .map(|r| (r.def.method, r.def.path))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Joins a prefix and a path into a `'static` template.
///
/// Route templates are `&'static str` so they can be used as metric labels
/// without allocating per request. Nesting has to produce new templates, so
/// they are leaked — bounded by the number of routes, and only at startup.
fn join_path(prefix: &str, path: &str) -> &'static str {
    let prefix = prefix.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path
    } else {
        return Box::leak(format!("{prefix}/{path}").into_boxed_str());
    };
    if prefix.is_empty() {
        return Box::leak(path.to_owned().into_boxed_str());
    }
    Box::leak(format!("{prefix}{path}").into_boxed_str())
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds routes, typically from `routes![...]`.
    pub fn mount<I, R>(mut self, routes: I) -> Self
    where
        I: IntoIterator<Item = R>,
        R: IntoRoute,
    {
        for route in routes {
            self.routes.push(RouteEntry {
                def: route.into_route(),
                tags: Vec::new(),
                middleware: Vec::new(),
            });
        }
        self
    }

    /// Adds routes under a path prefix.
    pub fn mount_at<I, R>(self, prefix: &str, routes: I) -> Self
    where
        I: IntoIterator<Item = R>,
        R: IntoRoute,
    {
        self.nest(prefix, Router::new().mount(routes))
    }

    /// Applies a middleware layer to every route added to this router,
    /// including ones nested inside it.
    pub fn layer(mut self, middleware: Arc<dyn Middleware>) -> Self {
        self.middleware.push(middleware);
        self
    }

    /// Tags every route currently in this router.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        let tag = tag.into();
        for entry in &mut self.routes {
            if !entry.tags.contains(&tag) {
                entry.tags.push(tag.clone());
            }
        }
        self
    }

    /// Mounts another router under `prefix`, inheriting its tags and layers.
    pub fn nest(mut self, prefix: &str, other: Router) -> Self {
        for mut entry in other.routes {
            entry.def.path = join_path(prefix, entry.def.path);
            // The nested router's own layers sit inside this router's.
            let mut middleware = other.middleware.clone();
            middleware.extend(entry.middleware);
            entry.middleware = middleware;
            self.routes.push(entry);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }
}

// `Sync` as well as `Send`: the built `Service` is shared across connection
// tasks behind an `Arc`, and a non-`Sync` field would make every request future
// non-`Send`.
type StartupHook =
    Box<dyn FnOnce(Arc<StateMap>) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;
type ShutdownHook = Box<dyn FnOnce(Arc<StateMap>) -> BoxFuture<'static, ()> + Send + Sync>;

/// The application: routes, state, documentation, and server settings.
pub struct App {
    router: Router,
    info: Info,
    servers: Vec<Server>,
    tags: Vec<Tag>,
    security_schemes: Map<SecurityScheme>,
    state: StateMap,
    docs: Option<Docs>,
    body_limit: usize,
    catch_panics: bool,
    startup: Vec<StartupHook>,
    shutdown: Vec<ShutdownHook>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("title", &self.info.title)
            .field("version", &self.info.version)
            .field("routes", &self.router.len())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            info: Info {
                title: "velo".into(),
                version: "0.1.0".into(),
                ..Default::default()
            },
            servers: Vec::new(),
            tags: Vec::new(),
            security_schemes: Map::new(),
            state: StateMap::new(),
            docs: Some(Docs::default()),
            body_limit: DEFAULT_BODY_LIMIT,
            catch_panics: true,
            startup: Vec::new(),
            shutdown: Vec::new(),
        }
    }

    // ---- document metadata ----------------------------------------------

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.info.title = title.into();
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.info.version = version.into();
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.info.description = Some(description.into());
        self
    }

    /// Replaces the whole `info` block.
    pub fn info(mut self, info: Info) -> Self {
        self.info = info;
        self
    }

    /// Adds a server entry to the document.
    pub fn server(mut self, url: impl Into<String>, description: Option<&str>) -> Self {
        self.servers.push(Server {
            url: url.into(),
            description: description.map(str::to_owned),
            variables: Map::new(),
        });
        self
    }

    /// Documents a tag. Tags used by routes but never described here still
    /// appear; this only adds the prose.
    pub fn tag(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        self.tags.push(Tag {
            name: name.into(),
            description: Some(description.into()),
            external_docs: None,
        });
        self
    }

    /// Registers a security scheme under a name.
    pub fn security_scheme(mut self, name: impl Into<String>, scheme: SecurityScheme) -> Self {
        self.security_schemes.insert(name.into(), scheme);
        self
    }

    // ---- wiring -----------------------------------------------------------

    /// Registers a value retrievable with `State<T>`.
    pub fn with_state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.state.insert(value);
        self
    }

    /// Registers an already-shared value.
    pub fn with_shared_state<T: Send + Sync + 'static>(mut self, value: Arc<T>) -> Self {
        self.state.insert_arc(value);
        self
    }

    pub fn mount<I, R>(mut self, routes: I) -> Self
    where
        I: IntoIterator<Item = R>,
        R: IntoRoute,
    {
        self.router = self.router.mount(routes);
        self
    }

    pub fn mount_at<I, R>(mut self, prefix: &str, routes: I) -> Self
    where
        I: IntoIterator<Item = R>,
        R: IntoRoute,
    {
        self.router = self.router.mount_at(prefix, routes);
        self
    }

    pub fn nest(mut self, prefix: &str, router: Router) -> Self {
        self.router = self.router.nest(prefix, router);
        self
    }

    /// Applies a layer to every route.
    pub fn layer(mut self, middleware: Arc<dyn Middleware>) -> Self {
        self.router = self.router.layer(middleware);
        self
    }

    /// Configures the documentation endpoints.
    pub fn docs(mut self, docs: Docs) -> Self {
        self.docs = Some(docs);
        self
    }

    /// Serves no documentation endpoints. The document is still available
    /// programmatically via [`App::openapi`].
    pub fn without_docs(mut self) -> Self {
        self.docs = None;
        self
    }

    /// Sets the maximum buffered request body size.
    pub fn body_limit(mut self, bytes: usize) -> Self {
        self.body_limit = bytes;
        self
    }

    /// Turns off the panic-catching layer, letting panics propagate.
    pub fn without_panic_guard(mut self) -> Self {
        self.catch_panics = false;
        self
    }

    /// Runs before the listener accepts connections. Returning `Err` aborts
    /// startup, which is the point: a service that cannot reach its database
    /// should fail to start rather than serve 500s.
    pub fn on_startup<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce(Arc<StateMap>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        self.startup
            .push(Box::new(move |state| Box::pin(hook(state))));
        self
    }

    /// Runs after the server has stopped accepting and in-flight requests have
    /// drained.
    pub fn on_shutdown<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce(Arc<StateMap>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.shutdown
            .push(Box::new(move |state| Box::pin(hook(state))));
        self
    }

    // ---- documentation ----------------------------------------------------

    /// Builds the OpenAPI document from the mounted routes.
    ///
    /// This walks the same `describe` hooks the server uses, so what you get
    /// here is exactly what `/openapi.json` serves.
    pub fn openapi(&self) -> OpenApi {
        let mut generator = SchemaGenerator::new();
        let mut paths: Map<PathItem> = Map::new();

        for entry in &self.router.routes {
            let mut operation = Operation::default();
            {
                let mut ctx = OperationContext {
                    generator: &mut generator,
                    operation: &mut operation,
                    path: entry.def.path,
                    method: entry.def.method,
                };
                (entry.def.describe)(&mut ctx);
            }

            for tag in &entry.tags {
                if !operation.tags.contains(tag) {
                    operation.tags.push(tag.clone());
                }
            }
            if operation.operation_id.is_none() {
                operation.operation_id =
                    Some(default_operation_id(entry.def.method, entry.def.path));
            }
            if operation.responses.is_empty() {
                operation
                    .responses
                    .insert("200".into(), velo_openapi::Response::new("Success"));
            }

            let documented_path = document_path(entry.def.path);
            let item = paths.entry(documented_path).or_default();
            if let Some(slot) = item.slot(entry.def.method) {
                *slot = Some(operation);
            }
        }

        let mut components = Components {
            schemas: generator.into_definitions(),
            security_schemes: self.security_schemes.clone(),
            ..Default::default()
        };

        // Any scheme an operation asked for that was not registered by hand:
        // supply the obvious definition rather than emitting a document that
        // dangles.
        for item in paths.values() {
            for operation in operations_of(item) {
                for requirement in operation.security.iter().flatten() {
                    for name in requirement.keys() {
                        if name == BEARER_SCHEME && !components.security_schemes.contains_key(name)
                        {
                            components
                                .security_schemes
                                .insert(name.clone(), SecurityScheme::bearer_jwt());
                        }
                    }
                }
            }
        }

        OpenApi {
            info: self.info.clone(),
            servers: self.servers.clone(),
            paths,
            components,
            tags: self.tags.clone(),
            ..Default::default()
        }
    }

    // ---- running ----------------------------------------------------------

    /// Compiles the routing table and documentation endpoints.
    pub fn build(self) -> Result<Service, RouteConflict> {
        let document = self.openapi();
        let App {
            mut router,
            state,
            docs,
            body_limit,
            catch_panics,
            startup,
            shutdown,
            ..
        } = self;

        if let Some(docs) = &docs {
            router = router.mount(docs.routes(&document));
        }

        let mut table = RouteTable::new();
        for entry in router.routes {
            // Global layers wrap per-router layers, which wrap the handler.
            let mut layers: Vec<Arc<dyn Middleware>> = Vec::new();
            if catch_panics {
                layers.push(Arc::new(CatchPanic));
            }
            layers.extend(router.middleware.iter().cloned());
            layers.extend(entry.middleware);
            table.insert(entry.def, &layers)?;
        }

        Ok(Service {
            table: Arc::new(table),
            state: Arc::new(state),
            body_limit,
            document: Arc::new(document),
            startup,
            shutdown,
        })
    }

    /// Binds and serves until the process receives Ctrl-C.
    pub async fn serve(self, addr: impl Into<SocketAddr>) -> std::io::Result<()> {
        self.serve_with_shutdown(addr, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }

    /// Binds and serves until `shutdown` resolves, then drains in-flight work.
    pub async fn serve_with_shutdown<F>(
        self,
        addr: impl Into<SocketAddr>,
        shutdown: F,
    ) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let service = self
            .build()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;
        service.serve_with_shutdown(addr.into(), shutdown).await
    }
}

/// Every operation present on a path item.
fn operations_of(item: &PathItem) -> impl Iterator<Item = &Operation> {
    [
        item.get.as_ref(),
        item.put.as_ref(),
        item.post.as_ref(),
        item.delete.as_ref(),
        item.options.as_ref(),
        item.head.as_ref(),
        item.patch.as_ref(),
        item.trace.as_ref(),
    ]
    .into_iter()
    .flatten()
}

/// OpenAPI has no catch-all syntax, so `{*rest}` is documented as `{rest}`.
fn document_path(path: &str) -> String {
    path.replace("{*", "{")
}

/// `GET /users/{id}` becomes `get_users_by_id`.
fn default_operation_id(method: &str, path: &str) -> String {
    let mut id = method.to_ascii_lowercase();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        id.push('_');
        match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(param) => {
                id.push_str("by_");
                id.push_str(param.trim_start_matches('*'));
            }
            None => id.push_str(&segment.replace('-', "_")),
        }
    }
    id
}

/// A built application, ready to serve.
pub struct Service {
    table: Arc<RouteTable>,
    state: Arc<StateMap>,
    body_limit: usize,
    document: Arc<OpenApi>,
    startup: Vec<StartupHook>,
    shutdown: Vec<ShutdownHook>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service")
            .field("routes", &self.table.registered())
            .finish_non_exhaustive()
    }
}

impl Service {
    /// The document this service serves.
    pub fn openapi(&self) -> &OpenApi {
        &self.document
    }

    /// Every registered `(method, template)` pair.
    pub fn routes(&self) -> &[(&'static str, &'static str)] {
        self.table.registered()
    }

    /// Handles one already-buffered request. This is the entry point the test
    /// client uses, so tests exercise the real dispatch path.
    pub async fn handle(&self, head: http::request::Parts, body: Bytes) -> Response {
        let mut request = Request::new(head, body);
        request.state = Arc::clone(&self.state);

        let path = request.path().to_owned();
        let method = request.method().clone();

        match self.table.find(&path, method.as_str()) {
            Match::Found { endpoint, params } => {
                request.params = params;
                request.matched_path = Some(endpoint.matched_path);
                (endpoint.handler)(request).await
            }
            Match::MethodNotAllowed(allowed) => {
                // A HEAD with no explicit route is answered by the GET handler
                // with the body dropped, which is what RFC 9110 requires.
                if method == Method::HEAD && allowed.contains(&"GET") {
                    if let Match::Found { endpoint, params } = self.table.find(&path, "GET") {
                        request.params = params;
                        request.matched_path = Some(endpoint.matched_path);
                        let mut response = (endpoint.handler)(request).await;
                        *response.body_mut() = ResBody::Empty;
                        return response;
                    }
                }
                method_not_allowed(&method, &allowed)
            }
            Match::NotFound => {
                ApiError::not_found(format!("No route for `{method} {path}`.")).into_response()
            }
        }
    }

    /// Runs the startup hooks and binds a listener, without accepting yet.
    ///
    /// Separating this from serving is what lets a test bind port 0 and then
    /// discover the port, and lets a supervisor know the process is ready
    /// before traffic arrives.
    pub async fn bind(mut self, addr: SocketAddr) -> std::io::Result<Bound> {
        for hook in std::mem::take(&mut self.startup) {
            hook(Arc::clone(&self.state))
                .await
                .map_err(|e| std::io::Error::other(format!("startup failed: {e}")))?;
        }

        let listener = tokio::net::TcpListener::bind(addr).await?;
        Ok(Bound {
            service: self,
            listener,
        })
    }

    /// Binds and serves until `shutdown` resolves.
    pub async fn serve_with_shutdown<F>(self, addr: SocketAddr, shutdown: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.bind(addr).await?.serve_with_shutdown(shutdown).await
    }
}

/// A [`Service`] with a listener already bound.
#[derive(Debug)]
pub struct Bound {
    service: Service,
    listener: tokio::net::TcpListener,
}

impl Bound {
    /// The address actually bound, which is how you learn the port after
    /// binding to `:0`.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// The service being served.
    pub fn service(&self) -> &Service {
        &self.service
    }

    /// Accepts connections until `shutdown` resolves, then drains.
    pub async fn serve_with_shutdown<F>(self, shutdown: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let Bound {
            mut service,
            listener,
        } = self;
        let local_addr = listener.local_addr()?;

        #[cfg(feature = "tracing")]
        tracing::info!(%local_addr, routes = service.table.len(), "velo listening");
        #[cfg(not(feature = "tracing"))]
        eprintln!(
            "velo listening on http://{local_addr} ({} routes)",
            service.table.len()
        );

        let shutdown_hooks = std::mem::take(&mut service.shutdown);
        let state = Arc::clone(&service.state);
        let service = Arc::new(service);

        let graceful = hyper_util::server::graceful::GracefulShutdown::new();
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(pair) => pair,
                        // A single failed accept (fd exhaustion, a peer that
                        // vanished) must not tear down the whole server.
                        Err(error) => {
                            #[cfg(feature = "tracing")]
                            tracing::warn!(%error, "accept failed");
                            let _ = &error;
                            continue;
                        }
                    };
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let service = Arc::clone(&service);
                    let builder = hyper_util::server::conn::auto::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    );
                    let connection = builder
                        .serve_connection_with_upgrades(io, HyperService { service, peer });
                    let watched = graceful.watch(connection.into_owned());
                    tokio::spawn(async move {
                        let _ = watched.await;
                    });
                }
                _ = shutdown.as_mut() => {
                    #[cfg(feature = "tracing")]
                    tracing::info!("shutdown signal received; draining");
                    break;
                }
            }
        }

        graceful.shutdown().await;

        for hook in shutdown_hooks {
            hook(Arc::clone(&state)).await;
        }
        Ok(())
    }
}

fn method_not_allowed(method: &Method, allowed: &[&'static str]) -> Response {
    let allow = allowed.join(", ");

    // A bare OPTIONS is a capability query, not an error.
    if method == Method::OPTIONS {
        let mut response = Response::new(ResBody::Empty);
        *response.status_mut() = StatusCode::NO_CONTENT;
        if let Ok(value) = HeaderValue::from_str(&allow) {
            response.headers_mut().insert(header::ALLOW, value);
        }
        return response;
    }

    let mut response = ApiError::new(StatusCode::METHOD_NOT_ALLOWED)
        .with_detail(format!("`{method}` is not supported here; try {allow}."))
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&allow) {
        response.headers_mut().insert(header::ALLOW, value);
    }
    response
}

/// Adapts [`Service`] to hyper.
#[derive(Clone)]
struct HyperService {
    service: Arc<Service>,
    peer: SocketAddr,
}

impl hyper::service::Service<http::Request<hyper::body::Incoming>> for HyperService {
    type Response = Response;
    type Error = std::convert::Infallible;
    type Future = BoxFuture<'static, Result<Response, Self::Error>>;

    fn call(&self, req: http::Request<hyper::body::Incoming>) -> Self::Future {
        let service = Arc::clone(&self.service);
        let peer = self.peer;

        Box::pin(async move {
            let (mut head, body) = req.into_parts();
            head.extensions.insert(ConnectInfo(peer));

            let body = match collect_limited(body, service.body_limit).await {
                Ok(bytes) => bytes,
                Err(response) => return Ok(response),
            };

            Ok(service.handle(head, body).await)
        })
    }
}

/// Reads a body, refusing anything over `limit` without buffering it first.
async fn collect_limited(body: hyper::body::Incoming, limit: usize) -> Result<Bytes, Response> {
    use http_body_util::BodyExt;
    use hyper::body::Body as _;

    // A declared length over the limit is refused before a single byte is
    // read, which is the difference between rejecting an upload and buffering
    // it in order to reject it.
    if let Some(hint) = body.size_hint().upper() {
        if hint > limit as u64 {
            return Err(too_large(limit));
        }
    }

    let collected = http_body_util::Limited::new(body, limit).collect().await;
    match collected {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(_) => Err(too_large(limit)),
    }
}

fn too_large(limit: usize) -> Response {
    ApiError::payload_too_large(format!("The request body exceeds the {limit}-byte limit."))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_are_derived_from_the_route() {
        assert_eq!(default_operation_id("GET", "/users"), "get_users");
        assert_eq!(
            default_operation_id("GET", "/users/{id}"),
            "get_users_by_id"
        );
        assert_eq!(
            default_operation_id("DELETE", "/orgs/{org}/api-keys/{id}"),
            "delete_orgs_by_org_api_keys_by_id"
        );
    }

    #[test]
    fn catch_alls_are_documented_without_the_star() {
        assert_eq!(document_path("/files/{*path}"), "/files/{path}");
    }

    #[test]
    fn prefixes_join_without_doubling_slashes() {
        assert_eq!(join_path("/api", "/users"), "/api/users");
        assert_eq!(join_path("/api/", "/users"), "/api/users");
        assert_eq!(join_path("", "/users"), "/users");
    }

    #[test]
    fn nesting_composes_prefixes() {
        let inner = Router::new().mount(vec![RouteDef::raw("GET", "/{id}", |_| async {
            Response::new(ResBody::Empty)
        })]);
        let outer = Router::new().nest("/users", inner);
        let app = App::new().nest("/api/v1", outer);
        let service = app.without_docs().build().unwrap();
        assert_eq!(service.routes(), &[("GET", "/api/v1/users/{id}")]);
    }
}
