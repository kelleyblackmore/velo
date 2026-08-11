//! Turning handler return values into HTTP responses.

use crate::body::ResBody;
use crate::error::{ApiError, ProblemDetails};
use crate::operation::{OperationContext, OperationOutput};
use http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use velo_openapi::{JsonSchema, MediaType, Schema};

/// The response type produced by every handler.
pub type Response = http::Response<ResBody>;

/// A type that can become an HTTP response.
///
/// Implementing this is how you teach `velo` about a custom response shape.
/// Pair it with [`OperationOutput`] so the documentation stays in step.
pub trait IntoResponse {
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for ResBody {
    fn into_response(self) -> Response {
        Response::new(self)
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        let mut response = Response::new(ResBody::Empty);
        *response.status_mut() = self;
        response
    }
}

impl IntoResponse for () {
    fn into_response(self) -> Response {
        let mut response = Response::new(ResBody::Empty);
        *response.status_mut() = StatusCode::NO_CONTENT;
        response
    }
}

impl OperationOutput for () {
    fn describe(ctx: &mut OperationContext<'_>) {
        ctx.add_response(204, velo_openapi::Response::new("No content"));
    }
    fn status() -> Option<u16> {
        Some(204)
    }
}

impl OperationOutput for StatusCode {
    fn describe(ctx: &mut OperationContext<'_>) {
        ctx.add_response("default", velo_openapi::Response::new("Status only"));
    }
    fn status() -> Option<u16> {
        None
    }
}

macro_rules! text_response {
    ($ty:ty) => {
        impl IntoResponse for $ty {
            fn into_response(self) -> Response {
                let mut response = Response::new(ResBody::from(self));
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                response
            }
        }

        impl OperationOutput for $ty {
            fn describe(ctx: &mut OperationContext<'_>) {
                let mut response = velo_openapi::Response::new("Success");
                response.content.insert(
                    "text/plain".into(),
                    MediaType::new(Schema::of_type("string")),
                );
                ctx.add_response(200, response);
            }
        }
    };
}

text_response!(String);
text_response!(&'static str);

impl IntoResponse for bytes::Bytes {
    fn into_response(self) -> Response {
        let mut response = Response::new(ResBody::full(self));
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        response
    }
}

impl OperationOutput for bytes::Bytes {
    fn describe(ctx: &mut OperationContext<'_>) {
        let mut response = velo_openapi::Response::new("Binary payload");
        response.content.insert(
            "application/octet-stream".into(),
            MediaType::new(Schema::typed("string", "binary")),
        );
        ctx.add_response(200, response);
    }
}

impl IntoResponse for Vec<u8> {
    fn into_response(self) -> Response {
        bytes::Bytes::from(self).into_response()
    }
}

impl OperationOutput for Vec<u8> {
    fn describe(ctx: &mut OperationContext<'_>) {
        <bytes::Bytes as OperationOutput>::describe(ctx)
    }
}

impl<T: IntoResponse, E: IntoResponse> IntoResponse for Result<T, E> {
    fn into_response(self) -> Response {
        match self {
            Ok(value) => value.into_response(),
            Err(error) => error.into_response(),
        }
    }
}

/// `Result` documents the success type's responses plus the error type's.
impl<T: OperationOutput, E: OperationOutput> OperationOutput for Result<T, E> {
    fn describe(ctx: &mut OperationContext<'_>) {
        T::describe(ctx);
        E::describe(ctx);
    }
    fn status() -> Option<u16> {
        T::status()
    }
}

/// Any error path through an `ApiError` is documented once, as a `default`
/// problem-details response, rather than guessing at every status a handler
/// might return.
impl OperationOutput for ApiError {
    fn describe(ctx: &mut OperationContext<'_>) {
        let schema = ctx.schema_for::<ProblemDetails>();
        let mut response = velo_openapi::Response::new("Error (RFC 9457 problem details)");
        response
            .content
            .insert(crate::error::PROBLEM_JSON.into(), MediaType::new(schema));
        ctx.add_response("default", response);
    }
    fn status() -> Option<u16> {
        None
    }
}

// ---- wrappers -------------------------------------------------------------

/// Overrides the status code of an inner response.
#[derive(Clone, Copy, Debug)]
pub struct WithStatus<T>(pub StatusCode, pub T);

impl<T: IntoResponse> IntoResponse for WithStatus<T> {
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        *response.status_mut() = self.0;
        response
    }
}

impl<T: OperationOutput> OperationOutput for WithStatus<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        T::describe(ctx)
    }
    fn status() -> Option<u16> {
        None
    }
}

/// `(StatusCode, T)` is the terse form of [`WithStatus`].
impl<T: IntoResponse> IntoResponse for (StatusCode, T) {
    fn into_response(self) -> Response {
        WithStatus(self.0, self.1).into_response()
    }
}

impl<T: OperationOutput> OperationOutput for (StatusCode, T) {
    fn describe(ctx: &mut OperationContext<'_>) {
        T::describe(ctx)
    }
    fn status() -> Option<u16> {
        None
    }
}

/// Attaches extra headers to an inner response.
#[derive(Clone, Debug)]
pub struct WithHeaders<T>(pub HeaderMap, pub T);

impl<T> WithHeaders<T> {
    /// Builds from an iterator of name/value pairs, skipping any that are not
    /// valid header syntax.
    pub fn from_pairs<I, K, V>(pairs: I, inner: T) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            if let (Ok(name), Ok(value)) = (
                HeaderName::try_from(k.as_ref()),
                HeaderValue::from_str(v.as_ref()),
            ) {
                map.insert(name, value);
            }
        }
        Self(map, inner)
    }
}

impl<T: IntoResponse> IntoResponse for WithHeaders<T> {
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        for (name, value) in self.0.iter() {
            response.headers_mut().insert(name.clone(), value.clone());
        }
        response
    }
}

impl<T: OperationOutput> OperationOutput for WithHeaders<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        T::describe(ctx)
    }
    fn status() -> Option<u16> {
        T::status()
    }
}

/// `201 Created`, optionally with a `Location` header.
#[derive(Clone, Debug)]
pub struct Created<T> {
    pub body: T,
    pub location: Option<String>,
}

impl<T> Created<T> {
    pub fn new(body: T) -> Self {
        Self {
            body,
            location: None,
        }
    }

    /// `201` plus `Location: <uri>`.
    pub fn at(location: impl Into<String>, body: T) -> Self {
        Self {
            body,
            location: Some(location.into()),
        }
    }
}

impl<T: IntoResponse> IntoResponse for Created<T> {
    fn into_response(self) -> Response {
        let mut response = self.body.into_response();
        *response.status_mut() = StatusCode::CREATED;
        if let Some(location) = self.location {
            if let Ok(value) = HeaderValue::from_str(&location) {
                response.headers_mut().insert(header::LOCATION, value);
            }
        }
        response
    }
}

impl<T: OperationOutput> OperationOutput for Created<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        // Let the inner type register at its own status, then move it to 201
        // so the document shows the status that is actually sent.
        let before: Vec<String> = ctx.operation.responses.keys().cloned().collect();
        T::describe(ctx);
        let added: Vec<String> = ctx
            .operation
            .responses
            .keys()
            .filter(|k| !before.contains(k))
            .cloned()
            .collect();
        for key in added {
            if key == "default" {
                continue;
            }
            if let Some(mut response) = ctx.operation.responses.shift_remove(&key) {
                response.description = "Created".into();
                let mut headers = velo_openapi::Map::new();
                headers.insert(
                    "Location".into(),
                    velo_openapi::Header {
                        description: Some("URI of the newly created resource.".into()),
                        required: false,
                        schema: Some(Schema::typed("string", "uri-reference")),
                    },
                );
                response.headers = headers;
                ctx.add_response(201, response);
            }
        }
    }
    fn status() -> Option<u16> {
        Some(201)
    }
}

/// `202 Accepted` with a body.
#[derive(Clone, Debug)]
pub struct Accepted<T>(pub T);

impl<T: IntoResponse> IntoResponse for Accepted<T> {
    fn into_response(self) -> Response {
        WithStatus(StatusCode::ACCEPTED, self.0).into_response()
    }
}

impl<T: OperationOutput> OperationOutput for Accepted<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        T::describe(ctx)
    }
    fn status() -> Option<u16> {
        Some(202)
    }
}

/// `204 No Content`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoContent;

impl IntoResponse for NoContent {
    fn into_response(self) -> Response {
        StatusCode::NO_CONTENT.into_response()
    }
}

impl OperationOutput for NoContent {
    fn describe(ctx: &mut OperationContext<'_>) {
        ctx.add_response(204, velo_openapi::Response::new("No content"));
    }
    fn status() -> Option<u16> {
        Some(204)
    }
}

/// An HTML document.
#[derive(Clone, Debug)]
pub struct Html<T>(pub T);

impl<T: Into<ResBody>> IntoResponse for Html<T> {
    fn into_response(self) -> Response {
        let mut response = Response::new(self.0.into());
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        response
    }
}

impl<T> OperationOutput for Html<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let mut response = velo_openapi::Response::new("An HTML document");
        response.content.insert(
            "text/html".into(),
            MediaType::new(Schema::of_type("string")),
        );
        ctx.add_response(200, response);
    }
}

/// A `3xx` redirect.
#[derive(Clone, Debug)]
pub struct Redirect {
    status: StatusCode,
    location: String,
}

impl Redirect {
    /// `303 See Other` — the right answer after a successful POST.
    pub fn see_other(location: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SEE_OTHER,
            location: location.into(),
        }
    }
    /// `307 Temporary Redirect`, which preserves the method and body.
    pub fn temporary(location: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TEMPORARY_REDIRECT,
            location: location.into(),
        }
    }
    /// `308 Permanent Redirect`, which preserves the method and body.
    pub fn permanent(location: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PERMANENT_REDIRECT,
            location: location.into(),
        }
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        let mut response = Response::new(ResBody::Empty);
        *response.status_mut() = self.status;
        match HeaderValue::from_str(&self.location) {
            Ok(value) => {
                response.headers_mut().insert(header::LOCATION, value);
                response
            }
            // A location that cannot be a header value is a programming error,
            // and silently emitting a redirect to nowhere is worse than a 500.
            Err(_) => {
                ApiError::internal("redirect location is not a valid header value").into_response()
            }
        }
    }
}

impl OperationOutput for Redirect {
    fn describe(ctx: &mut OperationContext<'_>) {
        let mut response = velo_openapi::Response::new("Redirect");
        let mut headers = velo_openapi::Map::new();
        headers.insert(
            "Location".into(),
            velo_openapi::Header {
                description: Some("The URI to follow.".into()),
                required: true,
                schema: Some(Schema::typed("string", "uri-reference")),
            },
        );
        response.headers = headers;
        ctx.add_response(303, response);
    }
    fn status() -> Option<u16> {
        Some(303)
    }
}

/// Documents a handler as returning `T`'s schema without any wrapper, for
/// hand-written [`IntoResponse`] implementations.
#[derive(Clone, Copy, Debug)]
pub struct Documented<T>(pub std::marker::PhantomData<T>);

impl<T: JsonSchema> OperationOutput for Documented<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let schema = ctx.schema_for::<T>();
        ctx.add_response(
            200,
            velo_openapi::Response::new("Success").with_json(schema),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::OperationContext;
    use http_body_util::BodyExt;
    use velo_openapi::{Operation, SchemaGenerator};

    fn describe<T: OperationOutput>() -> Operation {
        let mut generator = SchemaGenerator::new();
        let mut operation = Operation::default();
        let mut ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path: "/x",
            method: "GET",
        };
        T::describe(&mut ctx);
        operation
    }

    #[test]
    fn unit_is_a_204() {
        let response = ().into_response();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.into_body().is_empty());
    }

    #[tokio::test]
    async fn strings_are_utf8_plain_text() {
        let response = "hi".into_response();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"hi");
    }

    #[test]
    fn created_sets_status_and_location() {
        let response = Created::at("/users/7", "body").into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()[header::LOCATION], "/users/7");
    }

    #[test]
    fn tuple_status_overrides_the_inner_status() {
        let response = (StatusCode::IM_A_TEAPOT, "short and stout").into_response();
        assert_eq!(response.status(), StatusCode::IM_A_TEAPOT);
    }

    #[test]
    fn result_documents_both_arms() {
        let operation = describe::<Result<String, ApiError>>();
        assert!(operation.responses.contains_key("200"));
        assert!(operation.responses.contains_key("default"));
        assert!(operation.responses["default"]
            .content
            .contains_key("application/problem+json"));
    }

    #[test]
    fn created_moves_the_inner_response_to_201() {
        let operation = describe::<Created<String>>();
        assert!(!operation.responses.contains_key("200"));
        let created = &operation.responses["201"];
        assert_eq!(created.description, "Created");
        assert!(created.headers.contains_key("Location"));
    }

    #[test]
    fn redirect_documents_its_location_header() {
        let operation = describe::<Redirect>();
        assert!(operation.responses["303"].headers["Location"].required);
    }

    #[test]
    fn invalid_redirect_target_fails_loudly() {
        let response = Redirect::see_other("bad\nlocation").into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
