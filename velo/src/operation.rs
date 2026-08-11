//! The bridge between a handler's Rust signature and its OpenAPI operation.
//!
//! This is the heart of the "docs can't drift" guarantee. Every extractor
//! implements [`OperationInput`] and every response type implements
//! [`OperationOutput`]; the routing macros call them for each argument and for
//! the return type. Changing a handler's types changes the document, because
//! there is only one place the information lives.

use velo_openapi::{Operation, Parameter, Response, Schema, SchemaGenerator};

#[cfg(test)]
use velo_openapi::ParameterIn;

/// Everything an input or output needs in order to describe itself.
#[derive(Debug)]
pub struct OperationContext<'a> {
    /// The shared schema generator; nested types are registered here.
    pub generator: &'a mut SchemaGenerator,
    /// The operation being built, mutated in place.
    pub operation: &'a mut Operation,
    /// The route template, e.g. `/users/{id}`.
    pub path: &'static str,
    /// The uppercase HTTP method.
    pub method: &'static str,
}

impl OperationContext<'_> {
    /// The parameter names in the route template, in order.
    ///
    /// `Path<T>` uses this to name its parameters, which is why
    /// `Path<u32>` on `/users/{id}` documents an `id` parameter without any
    /// annotation.
    pub fn path_param_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        let mut rest = self.path;
        while let Some(open) = rest.find('{') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('}') else { break };
            let raw = &rest[..close];
            names.push(raw.strip_prefix('*').unwrap_or(raw));
            rest = &rest[close + 1..];
        }
        names
    }

    /// Adds a parameter, replacing any existing one with the same name and
    /// location so later extractors refine rather than duplicate.
    pub fn add_parameter(&mut self, parameter: Parameter) {
        if let Some(existing) = self
            .operation
            .parameters
            .iter_mut()
            .find(|p| p.name == parameter.name && p.location == parameter.location)
        {
            *existing = parameter;
        } else {
            self.operation.parameters.push(parameter);
        }
    }

    /// Records a response, keyed by status code (or `"default"`).
    pub fn add_response(&mut self, status: impl ToString, response: Response) {
        self.operation
            .responses
            .insert(status.to_string(), response);
    }

    /// Records an error response documented as RFC 9457 problem details.
    pub fn add_problem_response(&mut self, status: impl ToString, description: impl Into<String>) {
        let response = problem_response(self.generator, description);
        self.add_response(status, response);
    }

    /// Records the `422` that a failed `#[validate(...)]` rule produces.
    pub fn add_validation_response(&mut self) {
        let response = validation_response(self.generator);
        self.add_response(422, response);
    }

    /// Registers `T` and returns a `$ref` to it.
    pub fn schema_for<T: velo_openapi::JsonSchema + ?Sized>(&mut self) -> Schema {
        self.generator.subschema_for::<T>()
    }

    /// Returns `T`'s schema fully expanded, for cases where the keywords must
    /// be readable in place (query and path parameters).
    pub fn inline_schema_for<T: velo_openapi::JsonSchema + ?Sized>(&mut self) -> Schema {
        self.generator.inline_for::<T>()
    }
}

/// A handler argument that contributes to the operation description.
pub trait OperationInput {
    /// Mutates the operation to account for this input. The default is a
    /// no-op, which is correct for things like `State<T>` that are invisible
    /// from outside the process.
    fn describe(ctx: &mut OperationContext<'_>) {
        let _ = ctx;
    }
}

/// A handler return type that contributes response documentation.
pub trait OperationOutput {
    /// Mutates the operation to account for this output.
    fn describe(ctx: &mut OperationContext<'_>);

    /// The status code this type produces on its own, when it is fixed.
    /// Wrappers like `Result<T, E>` consult it to place `T`'s schema correctly.
    fn status() -> Option<u16> {
        Some(200)
    }
}

/// Builds an error response whose body is documented as problem details.
///
/// Every error this framework produces has the same shape, so the document
/// should say so rather than leaving clients to guess per endpoint.
pub fn problem_response(
    generator: &mut SchemaGenerator,
    description: impl Into<String>,
) -> Response {
    let schema = generator.subschema_for::<crate::error::ProblemDetails>();
    let mut response = Response::new(description);
    response.content.insert(
        crate::error::PROBLEM_JSON.into(),
        velo_openapi::MediaType::new(schema),
    );
    response
}

/// The `422` a failed `#[validate(...)]` rule produces, including the
/// `errors` array that names each offending field.
pub fn validation_response(generator: &mut SchemaGenerator) -> Response {
    problem_response(
        generator,
        "Validation failed. `errors` lists every field that did not pass, \
         each with a JSON Pointer to the value.",
    )
}

/// Expands a schema that may be a bare `$ref` into an object with visible
/// properties, following the reference into the generator's definitions.
///
/// Query and path parameters need real keywords, not a pointer, because each
/// property becomes its own parameter.
pub(crate) fn resolve_object(generator: &SchemaGenerator, schema: &Schema) -> Schema {
    if let Some(reference) = &schema.reference {
        if let Some(name) = reference.strip_prefix("#/components/schemas/") {
            if let Some(resolved) = generator.definitions().get(name) {
                return resolved.clone();
            }
        }
    }
    schema.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_for(path: &'static str) -> (SchemaGenerator, Operation, &'static str) {
        (SchemaGenerator::new(), Operation::default(), path)
    }

    #[test]
    fn path_params_are_read_from_the_template() {
        let (mut generator, mut operation, path) = ctx_for("/orgs/{org}/users/{id}");
        let ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path,
            method: "GET",
        };
        assert_eq!(ctx.path_param_names(), vec!["org", "id"]);
    }

    #[test]
    fn catch_all_params_drop_the_star() {
        let (mut generator, mut operation, path) = ctx_for("/files/{*rest}");
        let ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path,
            method: "GET",
        };
        assert_eq!(ctx.path_param_names(), vec!["rest"]);
    }

    #[test]
    fn adding_a_parameter_twice_refines_instead_of_duplicating() {
        let (mut generator, mut operation, path) = ctx_for("/x");
        let mut ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path,
            method: "GET",
        };
        ctx.add_parameter(Parameter::new(
            "page",
            ParameterIn::Query,
            Schema::of_type("string"),
        ));
        ctx.add_parameter(Parameter::new(
            "page",
            ParameterIn::Query,
            Schema::of_type("integer"),
        ));
        assert_eq!(operation.parameters.len(), 1);
        assert_eq!(
            operation.parameters[0].schema.as_ref().unwrap().schema_type,
            Some("integer".into())
        );
    }
}
