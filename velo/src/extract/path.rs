//! Typed path parameters.

use crate::error::{ApiError, FieldError};
use crate::extract::de::Pairs;
use crate::extract::FromRequest;
use crate::operation::{OperationContext, OperationInput};
use crate::request::Request;
use serde::de::DeserializeOwned;
use std::ops::Deref;
use velo_openapi::{JsonSchema, Parameter, ParameterIn, Schema};

/// The parameters captured from the route template.
///
/// `Path<u32>` on `/users/{id}` extracts the single segment. `Path<(String,
/// u32)>` takes them positionally. `Path<MyStruct>` takes them by name — and
/// in every case the OpenAPI parameter list is generated from the same types,
/// so a renamed field cannot leave a stale parameter behind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Path<T>(pub T);

impl<T> Path<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Path<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> FromRequest for Path<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        let pairs: Pairs = req
            .params()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();

        if pairs.is_empty() {
            // Reaching here means the handler asked for path parameters on a
            // route template that declares none — a wiring bug, not a bad
            // request.
            return Err(ApiError::internal(format!(
                "handler expects path parameters but route `{}` declares none",
                req.matched_path().unwrap_or(req.path())
            )));
        }

        pairs.deserialize().map(Path).map_err(|error| {
            ApiError::unprocessable(vec![FieldError::new(
                "",
                "invalid_path_parameter",
                error.to_string(),
            )])
            .with_title("Invalid path parameter")
        })
    }
}

impl<T: JsonSchema> OperationInput for Path<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let names = ctx.path_param_names();
        if names.is_empty() {
            return;
        }

        let schema = ctx.inline_schema_for::<T>();
        let resolved = crate::operation::resolve_object(ctx.generator, &schema);

        if !resolved.properties.is_empty() {
            // A struct: match declared properties to template names, and keep
            // the template's order so the document reads like the URL.
            for name in names {
                let mut property = resolved
                    .properties
                    .get(name)
                    .cloned()
                    // A parameter in the template with no matching field is
                    // still a real parameter; document it as a string rather
                    // than omitting it.
                    .unwrap_or_else(|| Schema::of_type("string"));
                let description = property.description.take();
                let mut parameter = Parameter::new(name, ParameterIn::Path, property);
                parameter.description = description;
                ctx.add_parameter(parameter);
            }
        } else if names.len() == 1 {
            // A scalar newtype: the whole schema describes the one parameter.
            let mut schema = resolved.clone();
            let description = schema.description.take();
            let mut parameter = Parameter::new(names[0], ParameterIn::Path, schema);
            parameter.description = description;
            ctx.add_parameter(parameter);
        } else {
            // A tuple: positional, so element schemas line up with the
            // template order.
            for (index, name) in names.iter().enumerate() {
                let item = resolved
                    .prefix_items
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| Schema::of_type("string"));
                ctx.add_parameter(Parameter::new(*name, ParameterIn::Path, item));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_request;
    use velo_openapi::{Operation, SchemaGenerator};

    fn describe<T: OperationInput>(path: &'static str) -> Operation {
        let mut generator = SchemaGenerator::new();
        let mut operation = Operation::default();
        let mut ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path,
            method: "GET",
        };
        T::describe(&mut ctx);
        operation
    }

    #[tokio::test]
    async fn a_single_parameter_extracts_as_a_scalar() {
        let mut req = test_request().param("id", "42").build();
        let Path(id) = Path::<u32>::from_request(&mut req).await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn several_parameters_extract_as_a_tuple_in_template_order() {
        let mut req = test_request().param("org", "acme").param("id", "7").build();
        let Path((org, id)) = Path::<(String, u32)>::from_request(&mut req).await.unwrap();
        assert_eq!((org.as_str(), id), ("acme", 7));
    }

    #[tokio::test]
    async fn a_struct_extracts_by_name() {
        #[derive(Debug, serde::Deserialize, PartialEq)]
        struct Ids {
            org: String,
            id: u32,
        }
        let mut req = test_request().param("org", "acme").param("id", "7").build();
        let Path(ids) = Path::<Ids>::from_request(&mut req).await.unwrap();
        assert_eq!(
            ids,
            Ids {
                org: "acme".into(),
                id: 7
            }
        );
    }

    #[tokio::test]
    async fn an_unparseable_segment_is_a_422_not_a_500() {
        let mut req = test_request().param("id", "not-a-number").build();
        let err = Path::<u32>::from_request(&mut req).await.unwrap_err();
        assert_eq!(err.status(), http::StatusCode::UNPROCESSABLE_ENTITY);
        assert!(err.field_errors()[0].message.contains("not-a-number"));
    }

    #[test]
    fn scalar_paths_take_their_name_from_the_template() {
        let operation = describe::<Path<u32>>("/users/{id}");
        assert_eq!(operation.parameters.len(), 1);
        assert_eq!(operation.parameters[0].name, "id");
        assert!(operation.parameters[0].required);
        assert_eq!(
            operation.parameters[0].schema.as_ref().unwrap().format,
            Some("int64".into())
        );
    }

    #[test]
    fn tuple_paths_line_up_positionally() {
        let operation = describe::<Path<(String, u32)>>("/orgs/{org}/users/{id}");
        let names: Vec<_> = operation.parameters.iter().map(|p| &p.name).collect();
        assert_eq!(names, vec!["org", "id"]);
        assert_eq!(
            operation.parameters[1].schema.as_ref().unwrap().schema_type,
            Some("integer".into())
        );
    }
}
