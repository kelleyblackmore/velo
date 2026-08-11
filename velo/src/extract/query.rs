//! Typed query strings.

use crate::error::{ApiError, FieldError};
use crate::extract::de::Pairs;
use crate::extract::{parameters_from_object, FromRequest};
use crate::operation::{OperationContext, OperationInput};
use crate::request::Request;
use crate::validate::Validate;
use serde::de::DeserializeOwned;
use std::ops::Deref;
use velo_openapi::{JsonSchema, ParameterIn};

/// The parsed query string.
///
/// Repeated keys collect into sequences (`?tag=a&tag=b` → `Vec<String>`),
/// missing keys become `None`, and every field turns into a documented
/// parameter with its schema, constraints, and description intact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Query<T>(pub T);

impl<T> Query<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Query<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> FromRequest for Query<T>
where
    T: DeserializeOwned + Validate + Send,
{
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        let value: T = Pairs::parse_urlencoded(req.query())
            .deserialize()
            .map_err(|error| {
                ApiError::unprocessable(vec![FieldError::new(
                    "",
                    "invalid_query",
                    error.to_string(),
                )])
                .with_title("Invalid query string")
            })?;

        value.validate()?;
        Ok(Query(value))
    }
}

impl<T: JsonSchema> OperationInput for Query<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let schema = ctx.inline_schema_for::<T>();
        let resolved = crate::operation::resolve_object(ctx.generator, &schema);
        for parameter in parameters_from_object(&resolved, ParameterIn::Query) {
            ctx.add_parameter(parameter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_request;
    use serde::Deserialize;
    use velo_openapi::{Operation, Schema, SchemaGenerator};

    #[derive(Debug, Deserialize, PartialEq)]
    struct Search {
        q: String,
        #[serde(default)]
        tags: Vec<String>,
        limit: Option<u32>,
    }

    impl Validate for Search {}

    impl JsonSchema for Search {
        fn schema_name() -> Option<String> {
            Some("Search".into())
        }
        fn json_schema(generator: &mut SchemaGenerator) -> Schema {
            let mut schema = Schema::of_type("object");
            schema
                .properties
                .insert("q".into(), generator.subschema_for::<String>());
            schema
                .properties
                .insert("tags".into(), generator.subschema_for::<Vec<String>>());
            schema
                .properties
                .insert("limit".into(), generator.subschema_for::<Option<u32>>());
            schema.required = vec!["q".into()];
            schema
        }
    }

    #[tokio::test]
    async fn repeated_keys_collect_into_a_vec() {
        let mut req = test_request().uri("/s?q=rust&tags=web&tags=async").build();
        let Query(search) = Query::<Search>::from_request(&mut req).await.unwrap();
        assert_eq!(search.q, "rust");
        assert_eq!(search.tags, vec!["web", "async"]);
        assert_eq!(search.limit, None);
    }

    #[tokio::test]
    async fn a_missing_required_key_is_a_422() {
        let mut req = test_request().uri("/s?tags=web").build();
        let err = Query::<Search>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn every_field_becomes_a_documented_parameter() {
        let mut generator = SchemaGenerator::new();
        let mut operation = Operation::default();
        let mut ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path: "/s",
            method: "GET",
        };
        Query::<Search>::describe(&mut ctx);

        assert_eq!(operation.parameters.len(), 3);
        let q = &operation.parameters[0];
        assert_eq!(q.name, "q");
        assert_eq!(q.location, ParameterIn::Query);
        assert!(q.required, "`q` is in the schema's required list");
        assert!(!operation.parameters[2].required, "`limit` is optional");
    }
}
