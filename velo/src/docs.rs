//! Self-serving API documentation.
//!
//! Mounting an [`App`](crate::App) gives you `/openapi.json` and a browsable
//! UI with no extra wiring, because the document is generated from the same
//! route definitions the server dispatches on.

use crate::body::ResBody;
use crate::response::{IntoResponse, Response};
use crate::route::RouteDef;
use http::{header, HeaderValue};
use std::sync::Arc;
use velo_openapi::OpenApi;

/// Which documentation renderer to serve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renderer {
    /// Scalar — the modern default: fast, searchable, dark-mode aware.
    Scalar,
    /// Swagger UI — the one everyone recognises, with a "try it" console.
    SwaggerUi,
    /// Redoc — three-pane reference layout, good for large documents.
    Redoc,
}

impl Renderer {
    fn default_path(self) -> &'static str {
        match self {
            Renderer::Scalar => "/docs",
            Renderer::SwaggerUi => "/swagger",
            Renderer::Redoc => "/redoc",
        }
    }
}

/// Documentation endpoint configuration.
#[derive(Clone, Debug)]
pub struct Docs {
    openapi_path: String,
    uis: Vec<(String, Renderer)>,
    /// Where the renderer's assets come from. Point this at your own host to
    /// serve documentation from an air-gapped network.
    asset_base: AssetBase,
    title: Option<String>,
}

/// Where documentation UI assets are loaded from.
#[derive(Clone, Debug)]
pub enum AssetBase {
    /// Public CDNs, matching FastAPI's default.
    Cdn,
    /// A prefix you host yourself. Each renderer's script is looked up
    /// underneath it, e.g. `{prefix}/scalar.js`.
    SelfHosted(String),
}

impl Default for Docs {
    fn default() -> Self {
        Self {
            openapi_path: "/openapi.json".into(),
            uis: vec![
                ("/docs".into(), Renderer::Scalar),
                ("/redoc".into(), Renderer::Redoc),
            ],
            asset_base: AssetBase::Cdn,
            title: None,
        }
    }
}

impl Docs {
    /// The default set: Scalar at `/docs`, Redoc at `/redoc`, document at
    /// `/openapi.json`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves only the JSON document, with no UI.
    pub fn json_only() -> Self {
        Self {
            uis: Vec::new(),
            ..Self::default()
        }
    }

    /// Serves a single renderer at its conventional path.
    pub fn only(renderer: Renderer) -> Self {
        Self::only_at(renderer.default_path(), renderer)
    }

    /// Serves a single renderer at a path you choose.
    ///
    /// `/docs` is where people look regardless of which renderer is behind it,
    /// so swapping renderers should not mean moving the URL:
    ///
    /// ```ignore
    /// App::new().docs(Docs::only_at("/docs", Renderer::Redoc))
    /// ```
    pub fn only_at(path: impl Into<String>, renderer: Renderer) -> Self {
        Self {
            uis: vec![(path.into(), renderer)],
            ..Self::default()
        }
    }

    /// Adds a renderer at a specific path.
    pub fn with(mut self, path: impl Into<String>, renderer: Renderer) -> Self {
        self.uis.push((path.into(), renderer));
        self
    }

    /// Changes where the JSON document is served.
    pub fn openapi_path(mut self, path: impl Into<String>) -> Self {
        self.openapi_path = path.into();
        self
    }

    /// Loads UI assets from a prefix you control rather than a public CDN.
    pub fn self_hosted_assets(mut self, prefix: impl Into<String>) -> Self {
        self.asset_base = AssetBase::SelfHosted(prefix.into());
        self
    }

    /// Overrides the browser tab title, which defaults to the document title.
    pub fn page_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Builds the routes that serve this configuration.
    pub(crate) fn routes(&self, document: &OpenApi) -> Vec<RouteDef> {
        let mut routes = Vec::with_capacity(self.uis.len() + 1);

        let payload: Arc<[u8]> = match serde_json::to_vec_pretty(document) {
            Ok(bytes) => Arc::from(bytes.into_boxed_slice()),
            // Serialising our own document should be infallible; if it is not,
            // serving a broken document is worse than serving none.
            Err(error) => {
                let message = format!(r#"{{"error":"{error}"}}"#);
                Arc::from(message.into_bytes().into_boxed_slice())
            }
        };

        routes.push(RouteDef::raw(
            "GET",
            leak(&self.openapi_path),
            move |_req| {
                let payload = Arc::clone(&payload);
                async move {
                    let mut response =
                        Response::new(ResBody::full(bytes::Bytes::copy_from_slice(&payload)));
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    // The document changes only when the binary does.
                    response
                        .headers_mut()
                        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                    response
                }
            },
        ));

        let title = self
            .title
            .clone()
            .unwrap_or_else(|| document.info.title.clone());

        for (path, renderer) in &self.uis {
            let html: Arc<str> = Arc::from(render(
                *renderer,
                &title,
                &self.openapi_path,
                &self.asset_base,
            ));
            routes.push(RouteDef::raw("GET", leak(path), move |_req| {
                let html = Arc::clone(&html);
                async move { crate::response::Html(html.to_string()).into_response() }
            }));
        }

        routes
    }

    /// The paths this configuration occupies, so an app can report them.
    pub fn paths(&self) -> Vec<&str> {
        std::iter::once(self.openapi_path.as_str())
            .chain(self.uis.iter().map(|(p, _)| p.as_str()))
            .collect()
    }
}

/// Route templates are `&'static str`; documentation paths are configured at
/// startup and leaked once, which is bounded and never repeats.
fn leak(path: &str) -> &'static str {
    Box::leak(path.to_owned().into_boxed_str())
}

/// HTML-escapes text destined for an attribute or element body.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn asset(base: &AssetBase, renderer: Renderer) -> String {
    match base {
        AssetBase::Cdn => match renderer {
            Renderer::Scalar => "https://cdn.jsdelivr.net/npm/@scalar/api-reference".into(),
            Renderer::SwaggerUi => {
                "https://cdn.jsdelivr.net/npm/swagger-ui-dist/swagger-ui-bundle.js".into()
            }
            Renderer::Redoc => {
                "https://cdn.jsdelivr.net/npm/redoc/bundles/redoc.standalone.js".into()
            }
        },
        AssetBase::SelfHosted(prefix) => {
            let prefix = prefix.trim_end_matches('/');
            match renderer {
                Renderer::Scalar => format!("{prefix}/scalar.js"),
                Renderer::SwaggerUi => format!("{prefix}/swagger-ui-bundle.js"),
                Renderer::Redoc => format!("{prefix}/redoc.standalone.js"),
            }
        }
    }
}

fn render(renderer: Renderer, title: &str, spec_url: &str, base: &AssetBase) -> String {
    let title = escape(title);
    let spec_url = escape(spec_url);
    let script = escape(&asset(base, renderer));

    match renderer {
        Renderer::Scalar => format!(
            r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
  </head>
  <body>
    <script id="api-reference" data-url="{spec_url}"></script>
    <script src="{script}"></script>
  </body>
</html>"#
        ),
        Renderer::SwaggerUi => {
            let stylesheet = escape(&match base {
                AssetBase::Cdn => {
                    "https://cdn.jsdelivr.net/npm/swagger-ui-dist/swagger-ui.css".to_owned()
                }
                AssetBase::SelfHosted(prefix) => {
                    format!("{}/swagger-ui.css", prefix.trim_end_matches('/'))
                }
            });
            format!(
                r##"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    <link rel="stylesheet" href="{stylesheet}" />
  </head>
  <body>
    <div id="swagger-ui"></div>
    <script src="{script}"></script>
    <script>
      window.ui = SwaggerUIBundle({{
        url: "{spec_url}",
        dom_id: "#swagger-ui",
        deepLinking: true
      }});
    </script>
  </body>
</html>"##
            )
        }
        Renderer::Redoc => format!(
            r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    <style>body {{ margin: 0; padding: 0; }}</style>
  </head>
  <body>
    <redoc spec-url="{spec_url}"></redoc>
    <script src="{script}"></script>
  </body>
</html>"#
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_set_covers_json_and_two_uis() {
        let docs = Docs::default();
        assert_eq!(docs.paths(), vec!["/openapi.json", "/docs", "/redoc"]);
    }

    #[test]
    fn a_single_renderer_can_be_placed_at_any_path() {
        // `/docs` is where people look whichever renderer is behind it, so
        // choosing a renderer must not force the URL to move.
        let docs = Docs::only_at("/docs", Renderer::Redoc);
        assert_eq!(docs.paths(), vec!["/openapi.json", "/docs"]);

        // The conventional placement still works.
        let docs = Docs::only(Renderer::Redoc);
        assert_eq!(docs.paths(), vec!["/openapi.json", "/redoc"]);
    }

    #[test]
    fn json_only_serves_no_ui() {
        let docs = Docs::json_only();
        assert_eq!(docs.paths(), vec!["/openapi.json"]);
    }

    #[test]
    fn a_hostile_title_cannot_break_out_of_the_page() {
        let html = render(
            Renderer::Redoc,
            "</title><script>alert(1)</script>",
            "/openapi.json",
            &AssetBase::Cdn,
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn self_hosted_assets_replace_the_cdn() {
        let base = AssetBase::SelfHosted("/static/docs/".into());
        assert_eq!(asset(&base, Renderer::Scalar), "/static/docs/scalar.js");
        assert!(!render(Renderer::Scalar, "t", "/openapi.json", &base).contains("jsdelivr"));
    }
}
