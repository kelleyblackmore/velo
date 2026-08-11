//! JSON request bodies and responses.

use crate::body::ResBody;
use crate::error::{ApiError, FieldError};
use crate::extract::FromRequest;
use crate::operation::{OperationContext, OperationInput, OperationOutput};
use crate::request::Request;
use crate::response::{IntoResponse, Response};
use crate::validate::Validate;
use http::{header, HeaderValue};
use serde::{de::DeserializeOwned, Serialize};
use std::ops::{Deref, DerefMut};
use velo_openapi::{JsonSchema, MediaType, RequestBody};

/// A JSON body, in either direction.
///
/// As an argument it parses, validates, and documents the request body. As a
/// return value it serialises and documents the response. One type, one
/// schema, both directions — there is no way to document one shape and send
/// another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T> Json<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Json<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Json<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> From<T> for Json<T> {
    fn from(value: T) -> Self {
        Json(value)
    }
}

/// True for `application/json` and the `+json` structured suffix
/// (`application/merge-patch+json`, `application/vnd.api+json`, ...).
fn is_json_media_type(content_type: &str) -> bool {
    let ct = content_type.trim();
    ct.eq_ignore_ascii_case("application/json")
        || ct.eq_ignore_ascii_case("text/json")
        || ct
            .rsplit_once('+')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("json"))
}

/// Converts a `serde_path_to_error` dotted path into an RFC 6901 pointer.
fn to_json_pointer(path: &serde_path_to_error::Path) -> String {
    use serde_path_to_error::Segment;
    let mut pointer = String::new();
    for segment in path.iter() {
        match segment {
            Segment::Seq { index } => {
                pointer.push('/');
                pointer.push_str(&index.to_string());
            }
            Segment::Map { key } => {
                pointer.push('/');
                pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
            }
            // Enum and unknown segments have no pointer representation; the
            // parent location is still the most useful thing to report.
            Segment::Enum { .. } | Segment::Unknown => {}
        }
    }
    pointer
}

impl<T> FromRequest for Json<T>
where
    T: DeserializeOwned + Validate + Send,
{
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        match req.content_type() {
            Some(ct) if is_json_media_type(ct) => {}
            Some(other) => {
                return Err(ApiError::unsupported_media_type(format!(
                    "Expected `application/json`, got `{other}`."
                )))
            }
            None if req.body().is_empty() => {
                return Err(ApiError::bad_request("Expected a JSON request body.")
                    .with_title("Missing body"))
            }
            // A body with no Content-Type is accepted rather than rejected:
            // the payload is what matters, and a parse failure below will say
            // so precisely.
            None => {}
        }

        if req.body().is_empty() {
            return Err(
                ApiError::bad_request("Expected a JSON request body, but it was empty.")
                    .with_title("Missing body"),
            );
        }

        let deserializer = &mut serde_json::Deserializer::from_slice(req.body());
        let value: T = match serde_path_to_error::deserialize(deserializer) {
            Ok(value) => value,
            Err(error) => {
                let pointer = to_json_pointer(error.path());
                let inner = error.into_inner();
                // Syntax errors are the client sending something that is not
                // JSON at all (400); everything else is well-formed JSON that
                // does not fit the schema (422), which is a different fix.
                return Err(if inner.classify() == serde_json::error::Category::Syntax {
                    ApiError::bad_request(format!("Malformed JSON: {inner}"))
                        .with_title("Invalid JSON")
                } else {
                    ApiError::unprocessable(vec![FieldError::new(
                        pointer,
                        "invalid_type",
                        inner.to_string(),
                    )])
                });
            }
        };

        value.validate()?;
        Ok(Json(value))
    }
}

impl<T: JsonSchema> OperationInput for Json<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let schema = ctx.schema_for::<T>();
        let mut content = velo_openapi::Map::new();
        content.insert("application/json".into(), MediaType::new(schema));
        ctx.operation.request_body = Some(RequestBody {
            description: None,
            content,
            required: true,
        });
        ctx.add_validation_response();
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => {
                let mut response = Response::new(ResBody::full(bytes));
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                response
            }
            // Serialisation failing means the response type is broken, not the
            // request. Surfacing a 500 beats emitting truncated JSON.
            Err(error) => ApiError::internal(error)
                .with_detail("The response could not be serialised.")
                .into_response(),
        }
    }
}

impl<T: JsonSchema> OperationOutput for Json<T> {
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
    use crate::testing::test_request;
    use http::StatusCode;
    use http_body_util::BodyExt;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct Item {
        name: String,
        price: u32,
    }

    impl Validate for Item {}

    #[tokio::test]
    async fn valid_json_deserialises() {
        let mut req = test_request()
            .json(r#"{"name":"widget","price":10}"#)
            .build();
        let Json(item) = Json::<Item>::from_request(&mut req).await.unwrap();
        assert_eq!(
            item,
            Item {
                name: "widget".into(),
                price: 10
            }
        );
    }

    #[tokio::test]
    async fn wrong_content_type_is_a_415() {
        let mut req = test_request()
            .header("content-type", "text/plain")
            .body("{}")
            .build();
        let err = Json::<Item>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn structured_json_suffixes_are_accepted() {
        let mut req = test_request()
            .header("content-type", "application/vnd.api+json")
            .body(r#"{"name":"a","price":1}"#)
            .build();
        assert!(Json::<Item>::from_request(&mut req).await.is_ok());
    }

    #[tokio::test]
    async fn broken_syntax_is_a_400() {
        let mut req = test_request().json("{not json").build();
        let err = Json::<Item>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_wrong_type_is_a_422_pointing_at_the_field() {
        let mut req = test_request()
            .json(r#"{"name":"widget","price":"free"}"#)
            .build();
        let err = Json::<Item>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.field_errors()[0].pointer, "/price");
    }

    #[tokio::test]
    async fn nested_failures_get_a_full_pointer() {
        #[derive(Debug, Deserialize)]
        struct Order {
            #[allow(dead_code)]
            items: Vec<Item>,
        }
        impl Validate for Order {}

        let mut req = test_request()
            .json(r#"{"items":[{"name":"a","price":1},{"name":"b","price":"x"}]}"#)
            .build();
        let err = Json::<Order>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.field_errors()[0].pointer, "/items/1/price");
    }

    #[tokio::test]
    async fn an_empty_body_says_so() {
        let mut req = test_request().json("").build();
        let err = Json::<Item>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert!(err.detail().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn responses_are_application_json() {
        let response = Json(Item {
            name: "widget".into(),
            price: 10,
        })
        .into_response();
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], br#"{"name":"widget","price":10}"#);
    }

    #[test]
    fn media_type_matching_covers_the_structured_suffix() {
        assert!(is_json_media_type("application/json"));
        assert!(is_json_media_type("APPLICATION/JSON"));
        assert!(is_json_media_type("application/merge-patch+json"));
        assert!(!is_json_media_type("application/jsonp"));
        assert!(!is_json_media_type("text/plain"));
    }
}
