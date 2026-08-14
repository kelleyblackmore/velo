//! Errors as RFC 9457 problem details.
//!
//! Every failure a `velo` application produces — extractor rejections,
//! validation failures, handler errors, panics — is an [`ApiError`], and every
//! [`ApiError`] serialises to `application/problem+json`. That is a deliberate
//! upgrade over FastAPI's ad-hoc `{"detail": ...}`: clients get a stable,
//! standardised, machine-readable envelope, and the shape is described in the
//! generated OpenAPI document so it is never a surprise.

use crate::body::ResBody;
use crate::response::{IntoResponse, Response};
use http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use serde::Serialize;
use serde_json::Value;
use velo_openapi::{JsonSchema, Map, Schema, SchemaGenerator};

/// The media type mandated by RFC 9457.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// A single field-level failure, used for validation and deserialisation.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FieldError {
    /// JSON Pointer to the offending value, e.g. `/items/0/price`.
    pub pointer: String,
    /// A short machine-readable code such as `min_length` or `invalid_type`.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
}

impl FieldError {
    pub fn new(
        pointer: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            pointer: pointer.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

impl JsonSchema for FieldError {
    fn schema_name() -> Option<String> {
        Some("FieldError".into())
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = Schema::of_type("object");
        schema.description = Some("A single field-level failure.".into());
        schema.properties.insert(
            "pointer".into(),
            generator
                .subschema_for::<String>()
                .with_description("JSON Pointer to the offending value."),
        );
        schema.properties.insert(
            "code".into(),
            generator
                .subschema_for::<String>()
                .with_description("Machine-readable failure code."),
        );
        schema.properties.insert(
            "message".into(),
            generator
                .subschema_for::<String>()
                .with_description("Human-readable explanation."),
        );
        schema.required = vec!["pointer".into(), "code".into(), "message".into()];
        schema
    }
}

/// An error that knows how to render itself as an HTTP response.
///
/// Everything but the status lives behind a box, which keeps `ApiError` two
/// words wide. That matters more here than it looks: almost every function in
/// this crate returns `Result<T, ApiError>`, and an error type carried inline
/// would tax the success path of every one of them for the sake of a value
/// that is usually never constructed.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    inner: Box<Details>,
}

/// The cold half of an [`ApiError`].
#[derive(Debug)]
struct Details {
    type_uri: Option<String>,
    title: String,
    detail: Option<String>,
    instance: Option<String>,
    errors: Vec<FieldError>,
    extensions: Map<Value>,
    headers: HeaderMap,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ApiError {
    /// Builds an error with the given status. The title defaults to the
    /// canonical reason phrase.
    pub fn new(status: StatusCode) -> Self {
        Self {
            status,
            inner: Box::new(Details {
                title: status.canonical_reason().unwrap_or("Error").to_owned(),
                type_uri: None,
                detail: None,
                instance: None,
                errors: Vec::new(),
                extensions: Map::new(),
                headers: HeaderMap::new(),
                source: None,
            }),
        }
    }

    // ---- common statuses -------------------------------------------------

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST).with_detail(detail)
    }
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED).with_detail(detail)
    }
    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN).with_detail(detail)
    }
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND).with_detail(detail)
    }
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT).with_detail(detail)
    }
    pub fn unsupported_media_type(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE).with_detail(detail)
    }
    pub fn payload_too_large(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE).with_detail(detail)
    }
    pub fn too_many_requests(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS).with_detail(detail)
    }

    /// `422` with a list of field-level problems. This is what a failed
    /// `#[derive(Schema)]` validation produces.
    pub fn unprocessable(errors: Vec<FieldError>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY)
            .with_title("Validation failed")
            .with_detail(match errors.len() {
                1 => "1 field failed validation.".to_owned(),
                n => format!("{n} fields failed validation."),
            })
            .with_field_errors(errors)
    }

    /// `500`, carrying the underlying error as a `source` for logging while
    /// keeping it out of the response body.
    pub fn internal(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR)
            .with_detail("The server encountered an unexpected condition.")
            .with_source(source)
    }

    // ---- builders --------------------------------------------------------

    pub fn with_status(mut self, status: StatusCode) -> Self {
        // Keep the title in sync unless it was customised away from the
        // previous status' reason phrase.
        if Some(self.inner.title.as_str()) == self.status.canonical_reason() {
            self.inner.title = status.canonical_reason().unwrap_or("Error").to_owned();
        }
        self.status = status;
        self
    }
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.inner.title = title.into();
        self
    }
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.inner.detail = Some(detail.into());
        self
    }
    /// Sets the `type` URI that identifies this problem class.
    pub fn with_type(mut self, type_uri: impl Into<String>) -> Self {
        self.inner.type_uri = Some(type_uri.into());
        self
    }
    /// Sets the `instance` URI identifying this specific occurrence.
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.inner.instance = Some(instance.into());
        self
    }
    pub fn with_field_errors(mut self, errors: Vec<FieldError>) -> Self {
        self.inner.errors = errors;
        self
    }
    pub fn with_field_error(mut self, error: FieldError) -> Self {
        self.inner.errors.push(error);
        self
    }
    /// Adds a top-level extension member to the problem document.
    pub fn with_extension(mut self, key: impl Into<String>, value: Value) -> Self {
        self.inner.extensions.insert(key.into(), value);
        self
    }
    /// Adds a header to the eventual response, e.g. `WWW-Authenticate`.
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.inner.headers.insert(name, value);
        self
    }
    pub fn with_source(
        mut self,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        self.inner.source = Some(source.into());
        self
    }

    // ---- accessors -------------------------------------------------------

    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn title(&self) -> &str {
        &self.inner.title
    }
    pub fn detail(&self) -> Option<&str> {
        self.inner.detail.as_deref()
    }
    pub fn field_errors(&self) -> &[FieldError] {
        &self.inner.errors
    }

    /// The problem document as JSON, without the HTTP envelope.
    pub fn to_problem_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            Value::String(
                self.inner
                    .type_uri
                    .clone()
                    .unwrap_or_else(|| "about:blank".to_owned()),
            ),
        );
        map.insert("title".into(), Value::String(self.inner.title.clone()));
        map.insert("status".into(), Value::from(self.status.as_u16()));
        if let Some(detail) = &self.inner.detail {
            map.insert("detail".into(), Value::String(detail.clone()));
        }
        if let Some(instance) = &self.inner.instance {
            map.insert("instance".into(), Value::String(instance.clone()));
        }
        if !self.inner.errors.is_empty() {
            map.insert(
                "errors".into(),
                serde_json::to_value(&self.inner.errors).unwrap_or(Value::Null),
            );
        }
        for (k, v) in &self.inner.extensions {
            map.insert(k.clone(), v.clone());
        }
        Value::Object(map)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status.as_u16(), self.inner.title)?;
        if let Some(detail) = &self.inner.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source.as_ref().map(|e| &**e as _)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let payload = serde_json::to_vec(&self.to_problem_json())
            .unwrap_or_else(|_| br#"{"title":"Error","status":500}"#.to_vec());

        let mut response = Response::new(ResBody::full(payload));
        *response.status_mut() = self.status;
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
        for (name, value) in self.inner.headers.iter() {
            response.headers_mut().insert(name.clone(), value.clone());
        }
        // Stash the error so downstream middleware (logging, tracing) can
        // inspect the real cause rather than re-parsing the body.
        if let Some(source) = self.inner.source {
            response
                .extensions_mut()
                .insert(ErrorSource(std::sync::Arc::from(source)));
        }
        response
    }
}

/// Wrapper placed in response extensions so middleware can log the cause.
///
/// Shared rather than owned because `http::Extensions` requires `Clone`, and a
/// boxed error is not cloneable.
#[derive(Clone, Debug)]
pub struct ErrorSource(pub std::sync::Arc<dyn std::error::Error + Send + Sync>);

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::bad_request(format!("Malformed JSON: {e}"))
            .with_title("Invalid JSON")
            .with_source(e)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::internal(e)
    }
}

impl From<StatusCode> for ApiError {
    fn from(status: StatusCode) -> Self {
        ApiError::new(status)
    }
}

/// The problem-details envelope, exposed so it can be referenced from the
/// generated OpenAPI document.
#[derive(Debug)]
pub struct ProblemDetails;

impl JsonSchema for ProblemDetails {
    fn schema_name() -> Option<String> {
        Some("ProblemDetails".into())
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = Schema::of_type("object");
        schema.title = Some("Problem Details".into());
        schema.description = Some(
            "An RFC 9457 problem details object. Served as `application/problem+json`.".into(),
        );
        let string = generator.subschema_for::<String>();
        schema.properties.insert(
            "type".into(),
            string
                .clone()
                .with_description("URI identifying the problem type."),
        );
        schema.properties.insert(
            "title".into(),
            string
                .clone()
                .with_description("Short, human-readable summary."),
        );
        schema.properties.insert(
            "status".into(),
            generator
                .subschema_for::<u16>()
                .with_description("The HTTP status code."),
        );
        schema.properties.insert(
            "detail".into(),
            string
                .clone()
                .with_description("Explanation specific to this occurrence."),
        );
        schema.properties.insert(
            "instance".into(),
            string.with_description("URI identifying this occurrence."),
        );
        let field_error = generator.subschema_for::<FieldError>();
        schema.properties.insert(
            "errors".into(),
            Schema::array(field_error)
                .with_description("Field-level failures, present on validation errors."),
        );
        schema.required = vec!["title".into(), "status".into()];
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn problem_document_follows_rfc_9457() {
        let err = ApiError::not_found("No user with id 7").with_type("https://example.com/no-user");
        let doc = err.to_problem_json();
        assert_eq!(doc["type"], "https://example.com/no-user");
        assert_eq!(doc["title"], "Not Found");
        assert_eq!(doc["status"], 404);
        assert_eq!(doc["detail"], "No user with id 7");
    }

    #[test]
    fn type_defaults_to_about_blank() {
        assert_eq!(
            ApiError::bad_request("nope").to_problem_json()["type"],
            "about:blank"
        );
    }

    #[test]
    fn validation_errors_are_listed_and_counted() {
        let err = ApiError::unprocessable(vec![
            FieldError::new("/name", "min_length", "too short"),
            FieldError::new("/age", "minimum", "must be at least 13"),
        ]);
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let doc = err.to_problem_json();
        assert_eq!(doc["detail"], "2 fields failed validation.");
        assert_eq!(doc["errors"][0]["pointer"], "/name");
        assert_eq!(doc["errors"][1]["code"], "minimum");
    }

    #[test]
    fn changing_status_updates_a_default_title_but_not_a_custom_one() {
        let a = ApiError::new(StatusCode::OK).with_status(StatusCode::NOT_FOUND);
        assert_eq!(a.title(), "Not Found");
        let b = ApiError::new(StatusCode::OK)
            .with_title("Custom")
            .with_status(StatusCode::NOT_FOUND);
        assert_eq!(b.title(), "Custom");
    }

    #[tokio::test]
    async fn response_uses_the_problem_media_type() {
        let response = ApiError::forbidden("nope").into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let doc: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(doc["status"], 403);
    }

    #[test]
    fn internal_errors_keep_the_cause_out_of_the_body() {
        let err = ApiError::internal("database connection refused");
        let doc = err.to_problem_json();
        assert_eq!(
            doc["detail"],
            "The server encountered an unexpected condition."
        );
        assert!(!doc.to_string().contains("database"));
        assert!(std::error::Error::source(&err).is_some());
    }
}
