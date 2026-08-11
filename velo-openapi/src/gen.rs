//! The [`JsonSchema`] trait and the generator that collects reusable schemas
//! into `components/schemas`.

use crate::schema::Schema;
use crate::Map;

/// A Rust type that can describe itself as JSON Schema.
///
/// Implement this by hand for exotic types, or derive it with
/// `#[derive(Schema)]` from the `velo` crate.
pub trait JsonSchema {
    /// The name this type is registered under in `components/schemas`.
    ///
    /// Returning `None` inlines the schema at every use site, which is the
    /// right choice for primitives and transparent wrappers.
    fn schema_name() -> Option<String> {
        None
    }

    /// Builds the schema. Use [`SchemaGenerator::subschema_for`] for nested
    /// types so they are registered and referenced rather than duplicated.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema;

    /// `true` for `Option<T>`, which makes derived struct fields non-required.
    ///
    /// This is a associated const rather than a blanket specialisation because
    /// stable Rust has no way to detect `Option` generically.
    const OPTIONAL: bool = false;
}

/// Collects named schemas while walking a type graph.
///
/// The generator is cycle-safe: a type that (transitively) contains itself is
/// registered once and referenced thereafter.
#[derive(Debug, Default)]
pub struct SchemaGenerator {
    definitions: Map<Schema>,
    in_progress: Vec<String>,
}

impl SchemaGenerator {
    /// Creates an empty generator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the schema to use *at a use site* for `T`.
    ///
    /// Named types are registered in the component map and referenced;
    /// anonymous types are inlined.
    pub fn subschema_for<T: JsonSchema + ?Sized>(&mut self) -> Schema {
        match T::schema_name() {
            None => T::json_schema(self),
            Some(name) => {
                if !self.definitions.contains_key(&name)
                    && !self.in_progress.iter().any(|n| n == &name)
                {
                    // Mark before recursing so a self-referential type sees a
                    // `$ref` instead of recursing forever.
                    self.in_progress.push(name.clone());
                    let schema = T::json_schema(self);
                    self.in_progress.pop();
                    self.definitions.insert(name.clone(), schema);
                }
                Schema::reference(format!("#/components/schemas/{name}"))
            }
        }
    }

    /// Returns the schema for `T` fully inlined, still registering any nested
    /// named types it refers to.
    pub fn inline_for<T: JsonSchema + ?Sized>(&mut self) -> Schema {
        T::json_schema(self)
    }

    /// Registers a pre-built schema under an explicit name.
    pub fn insert(&mut self, name: impl Into<String>, schema: Schema) {
        self.definitions.insert(name.into(), schema);
    }

    /// All schemas collected so far, in insertion order.
    pub fn definitions(&self) -> &Map<Schema> {
        &self.definitions
    }

    /// Consumes the generator, yielding the collected schemas.
    pub fn into_definitions(self) -> Map<Schema> {
        self.definitions
    }
}

/// A usable identifier for `T`, for naming generic instantiations.
///
/// Named types use their component name; anonymous ones fall back to the last
/// segment of `type_name`, so `Page<u32>` becomes `Page_u32` rather than
/// something unprintable.
pub fn name_of<T: JsonSchema + ?Sized>() -> String {
    T::schema_name().unwrap_or_else(|| {
        let full = std::any::type_name::<T>();
        // Strip module paths and generic arguments down to a bare identifier.
        let base = full.split('<').next().unwrap_or(full);
        let last = base.rsplit("::").next().unwrap_or(base);
        let cleaned: String = last
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if cleaned.is_empty() {
            "Anonymous".to_owned()
        } else {
            cleaned
        }
    })
}

/// Builds a standalone document fragment for a single type. Handy in tests.
pub fn schema_for<T: JsonSchema>() -> (Schema, Map<Schema>) {
    let mut generator = SchemaGenerator::new();
    let root = generator.subschema_for::<T>();
    (root, generator.into_definitions())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Node;

    impl JsonSchema for Node {
        fn schema_name() -> Option<String> {
            Some("Node".into())
        }
        fn json_schema(generator: &mut SchemaGenerator) -> Schema {
            let mut s = Schema::of_type("object");
            // Self-reference: must not blow the stack.
            s.properties
                .insert("next".into(), generator.subschema_for::<Node>());
            s
        }
    }

    #[test]
    fn recursive_types_terminate_and_register_once() {
        let (root, defs) = schema_for::<Node>();
        assert_eq!(root.reference.as_deref(), Some("#/components/schemas/Node"));
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs["Node"].properties["next"].reference.as_deref(),
            Some("#/components/schemas/Node")
        );
    }
}
