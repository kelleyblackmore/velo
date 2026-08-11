//! Extractors: typed handler arguments.
//!
//! An extractor is a type that knows how to pull itself out of a request *and*
//! how to describe itself in the OpenAPI document. Those two jobs are
//! deliberately welded together — implementing [`FromRequest`] without
//! [`OperationInput`] will not compile into a route, so an argument can never
//! silently vanish from the docs.

mod de;
mod depends;
mod json;
mod path;
mod query;

pub use de::{DeError, Pairs};
pub use depends::{Dependency, Depends};
pub use json::Json;
pub use path::Path;
pub use query::Query;

use crate::error::ApiError;
use crate::operation::{OperationContext, OperationInput};
use crate::request::{ConnectInfo, Request};
use http::{header, HeaderMap, Method, Uri};
use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;
use velo_openapi::{MediaType, Parameter, ParameterIn, RequestBody, Schema};

/// A type that can be built from a request.
///
/// Extraction is asynchronous so that dependencies can do real work — hit a
/// database, verify a token — which is the piece FastAPI users actually miss
/// in other Rust frameworks.
pub trait FromRequest: Sized + Send {
    /// Pulls `Self` out of the request, or fails with a response-ready error.
    fn from_request(req: &mut Request) -> impl Future<Output = Result<Self, ApiError>> + Send;
}

/// Makes any extractor optional: a failure becomes `None` instead of a 4xx.
impl<T: FromRequest> FromRequest for Option<T> {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        Ok(T::from_request(req).await.ok())
    }
}

impl<T: OperationInput> OperationInput for Option<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let before = ctx.operation.parameters.len();
        T::describe(ctx);
        // Everything this extractor contributed is optional by construction.
        for parameter in &mut ctx.operation.parameters[before..] {
            if parameter.location != ParameterIn::Path {
                parameter.required = false;
            }
        }
        if let Some(body) = &mut ctx.operation.request_body {
            body.required = false;
        }
    }
}

/// Surfaces the rejection to the handler instead of short-circuiting.
impl<T: FromRequest> FromRequest for Result<T, ApiError> {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        Ok(T::from_request(req).await)
    }
}

impl<T: OperationInput> OperationInput for Result<T, ApiError> {
    fn describe(ctx: &mut OperationContext<'_>) {
        T::describe(ctx)
    }
}

// ---- request pieces -------------------------------------------------------

impl FromRequest for Method {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        Ok(req.method().clone())
    }
}
impl OperationInput for Method {}

impl FromRequest for Uri {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        Ok(req.uri().clone())
    }
}
impl OperationInput for Uri {}

impl FromRequest for HeaderMap {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        Ok(req.headers().clone())
    }
}
impl OperationInput for HeaderMap {}

impl FromRequest for bytes::Bytes {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        Ok(req.body_bytes())
    }
}

impl OperationInput for bytes::Bytes {
    fn describe(ctx: &mut OperationContext<'_>) {
        let mut content = velo_openapi::Map::new();
        content.insert(
            "application/octet-stream".into(),
            MediaType::new(Schema::typed("string", "binary")),
        );
        ctx.operation.request_body = Some(RequestBody {
            description: Some("Raw request body".into()),
            content,
            required: true,
        });
    }
}

impl FromRequest for String {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        std::str::from_utf8(req.body())
            .map(str::to_owned)
            .map_err(|e| {
                ApiError::bad_request(format!("Request body is not valid UTF-8: {e}"))
                    .with_title("Invalid encoding")
            })
    }
}

impl OperationInput for String {
    fn describe(ctx: &mut OperationContext<'_>) {
        let mut content = velo_openapi::Map::new();
        content.insert(
            "text/plain".into(),
            MediaType::new(Schema::of_type("string")),
        );
        ctx.operation.request_body = Some(RequestBody {
            description: Some("Plain text request body".into()),
            content,
            required: true,
        });
    }
}

impl FromRequest for ConnectInfo {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        req.peer_addr()
            .map(ConnectInfo)
            .ok_or_else(|| ApiError::internal("peer address is unavailable for this transport"))
    }
}
impl OperationInput for ConnectInfo {}

// ---- state and extensions -------------------------------------------------

/// Shared application state, registered with [`crate::App::with_state`].
///
/// Lookup is by type, and a missing type is a 500 with a message naming what
/// *was* registered — a wiring mistake should not read as a client error.
#[derive(Debug)]
pub struct State<T>(pub Arc<T>);

impl<T> Clone for State<T> {
    fn clone(&self) -> Self {
        State(Arc::clone(&self.0))
    }
}

impl<T> Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Send + Sync + 'static> FromRequest for State<T> {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        match req.state::<T>() {
            Some(value) => Ok(State(value)),
            None => Err(ApiError::internal(format!(
                "state type `{}` was never registered; registered types: [{}]",
                std::any::type_name::<T>(),
                req.state.registered().join(", ")
            ))),
        }
    }
}

impl<T> OperationInput for State<T> {}

/// A value placed in the request extensions by middleware.
#[derive(Clone, Copy, Debug)]
pub struct Extension<T>(pub T);

impl<T> Deref for Extension<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone + Send + Sync + 'static> FromRequest for Extension<T> {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        req.extensions()
            .get::<T>()
            .cloned()
            .map(Extension)
            .ok_or_else(|| {
                ApiError::internal(format!(
                    "no request extension of type `{}`; is the middleware that inserts it mounted?",
                    std::any::type_name::<T>()
                ))
            })
    }
}

impl<T> OperationInput for Extension<T> {}

// ---- headers --------------------------------------------------------------

/// A bearer token from the `Authorization` header.
///
/// Taking this as an argument also declares the `bearerAuth` security
/// requirement on the operation, and the app registers the scheme for you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bearer(pub String);

/// The security scheme name [`Bearer`] declares.
pub const BEARER_SCHEME: &str = "bearerAuth";

impl Deref for Bearer {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl FromRequest for Bearer {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        let unauthorized = |detail: &str| {
            ApiError::unauthorized(detail.to_owned()).with_header(
                header::WWW_AUTHENTICATE,
                http::HeaderValue::from_static("Bearer"),
            )
        };

        let raw = req
            .header(header::AUTHORIZATION)
            .ok_or_else(|| unauthorized("Missing Authorization header."))?;

        let (scheme, token) = raw
            .split_once(' ')
            .ok_or_else(|| unauthorized("Malformed Authorization header."))?;

        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(unauthorized("Expected a Bearer credential."));
        }
        let token = token.trim();
        if token.is_empty() {
            return Err(unauthorized("Bearer token is empty."));
        }
        Ok(Bearer(token.to_owned()))
    }
}

impl OperationInput for Bearer {
    fn describe(ctx: &mut OperationContext<'_>) {
        let mut requirement = velo_openapi::Map::new();
        requirement.insert(BEARER_SCHEME.to_owned(), Vec::new());
        ctx.operation
            .security
            .get_or_insert_with(Vec::new)
            .push(requirement);
        ctx.add_problem_response(401, "Missing or malformed bearer credential");
    }
}

/// The parsed `Cookie` header.
#[derive(Clone, Debug, Default)]
pub struct Cookies(Vec<(String, String)>);

impl Cookies {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Parses a `Cookie` header value.
    pub fn parse(raw: &str) -> Self {
        Cookies(
            raw.split(';')
                .filter_map(|pair| {
                    let pair = pair.trim();
                    if pair.is_empty() {
                        return None;
                    }
                    let (name, value) = pair.split_once('=')?;
                    let value = value.trim();
                    // Cookie values are commonly quoted; callers want the
                    // contents, not the quotes.
                    let value = value
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(value);
                    Some((name.trim().to_owned(), value.to_owned()))
                })
                .collect(),
        )
    }
}

impl FromRequest for Cookies {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        Ok(req
            .header(header::COOKIE)
            .map(Cookies::parse)
            .unwrap_or_default())
    }
}

impl OperationInput for Cookies {}

// ---- form bodies ----------------------------------------------------------

/// A `application/x-www-form-urlencoded` request body.
#[derive(Clone, Copy, Debug, Default)]
pub struct Form<T>(pub T);

impl<T> Deref for Form<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> FromRequest for Form<T>
where
    T: serde::de::DeserializeOwned + crate::validate::Validate + Send,
{
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        const EXPECTED: &str = "application/x-www-form-urlencoded";
        match req.content_type() {
            Some(ct) if ct.eq_ignore_ascii_case(EXPECTED) => {}
            Some(other) => {
                return Err(ApiError::unsupported_media_type(format!(
                    "Expected `{EXPECTED}`, got `{other}`."
                )))
            }
            None => {
                return Err(ApiError::unsupported_media_type(format!(
                    "Expected a `Content-Type` of `{EXPECTED}`."
                )))
            }
        }

        let body = std::str::from_utf8(req.body())
            .map_err(|e| ApiError::bad_request(format!("Form body is not valid UTF-8: {e}")))?;

        let value: T = Pairs::parse_urlencoded(body).deserialize().map_err(|e| {
            ApiError::unprocessable(vec![crate::error::FieldError::new(
                "",
                "invalid_form",
                e.to_string(),
            )])
        })?;

        value.validate()?;
        Ok(Form(value))
    }
}

impl<T: velo_openapi::JsonSchema> OperationInput for Form<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let schema = ctx.schema_for::<T>();
        let mut content = velo_openapi::Map::new();
        content.insert(
            "application/x-www-form-urlencoded".into(),
            MediaType::new(schema),
        );
        ctx.operation.request_body = Some(RequestBody {
            description: None,
            content,
            required: true,
        });
        ctx.add_validation_response();
    }
}

// ---- helpers used by the derived and hand-written extractors --------------

/// Builds a `Parameter` list from an object schema, one parameter per property.
pub(crate) fn parameters_from_object(schema: &Schema, location: ParameterIn) -> Vec<Parameter> {
    schema
        .properties
        .iter()
        .map(|(name, property)| {
            let mut property = property.clone();
            // The description belongs on the parameter; leaving a copy on the
            // schema makes every UI render it twice.
            let description = property.description.take();
            let mut parameter = Parameter::new(name.clone(), location, property);
            parameter.required =
                location == ParameterIn::Path || schema.required.iter().any(|r| r == name);
            parameter.description = description;
            parameter
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_request;
    use http::StatusCode;

    #[tokio::test]
    async fn bearer_rejects_the_wrong_scheme_with_a_challenge() {
        let mut req = test_request().header("authorization", "Basic abc").build();
        let err = Bearer::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_accepts_a_case_insensitive_scheme() {
        let mut req = test_request()
            .header("authorization", "bearer tok123")
            .build();
        assert_eq!(Bearer::from_request(&mut req).await.unwrap().0, "tok123");
    }

    #[tokio::test]
    async fn optional_extractors_swallow_rejections() {
        let mut req = test_request().build();
        let value = Option::<Bearer>::from_request(&mut req).await.unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn cookies_are_split_trimmed_and_unquoted() {
        let cookies = Cookies::parse("a=1; b = \"two\" ; c=");
        assert_eq!(cookies.get("a"), Some("1"));
        assert_eq!(cookies.get("b"), Some("two"));
        assert_eq!(cookies.get("c"), Some(""));
        assert_eq!(cookies.get("missing"), None);
    }

    #[tokio::test]
    async fn missing_state_is_a_500_that_names_the_type() {
        #[derive(Debug)]
        struct Db;
        let mut req = test_request().build();
        let err = State::<Db>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let source = std::error::Error::source(&err).unwrap().to_string();
        assert!(source.contains("Db"), "unhelpful: {source}");
    }

    #[tokio::test]
    async fn form_requires_the_right_content_type() {
        let mut req = test_request()
            .header("content-type", "application/json")
            .body("a=1")
            .build();
        let err = Form::<std::collections::HashMap<String, String>>::from_request(&mut req)
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
