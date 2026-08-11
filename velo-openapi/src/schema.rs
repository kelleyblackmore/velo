//! JSON Schema 2020-12 object model (the dialect OpenAPI 3.1 uses verbatim).

use crate::Map;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The `type` keyword, which in 2020-12 may be a single value or a list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaType {
    Single(String),
    Multiple(Vec<String>),
}

impl SchemaType {
    /// Adds `"null"` to this type, promoting a single type to a list.
    pub fn or_null(self) -> Self {
        match self {
            SchemaType::Single(t) if t == "null" => SchemaType::Single(t),
            SchemaType::Single(t) => SchemaType::Multiple(vec![t, "null".into()]),
            SchemaType::Multiple(mut v) => {
                if !v.iter().any(|t| t == "null") {
                    v.push("null".into());
                }
                SchemaType::Multiple(v)
            }
        }
    }
}

impl From<&str> for SchemaType {
    fn from(s: &str) -> Self {
        SchemaType::Single(s.to_owned())
    }
}

/// `additionalProperties` accepts either a boolean or a subschema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Bool(bool),
    Schema(Box<Schema>),
}

/// OpenAPI's polymorphism hint, used with `oneOf`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Discriminator {
    #[serde(rename = "propertyName")]
    pub property_name: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub mapping: Map<String>,
}

/// Serialises numeric keywords, preferring an integer when the value is whole.
mod number {
    use serde::Serializer;

    pub fn serialize<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            None => serializer.serialize_none(),
            Some(n) if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 => {
                serializer.serialize_i64(*n as i64)
            }
            Some(n) => serializer.serialize_f64(*n),
        }
    }
}

/// A JSON Schema.
///
/// Every keyword is optional; an all-default `Schema` serialises to `{}`, which
/// is the "accept anything" schema. Unknown/extension keywords survive a
/// round-trip through [`Schema::extensions`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    #[serde(rename = "$ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,

    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<SchemaType>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    #[serde(
        rename = "readOnly",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub read_only: bool,
    #[serde(
        rename = "writeOnly",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub write_only: bool,

    // ---- object ----
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub properties: Map<Schema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(
        rename = "additionalProperties",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_properties: Option<AdditionalProperties>,
    #[serde(
        rename = "minProperties",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub min_properties: Option<u64>,
    #[serde(
        rename = "maxProperties",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_properties: Option<u64>,

    // ---- array ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<Schema>>,
    #[serde(rename = "prefixItems", default, skip_serializing_if = "Vec::is_empty")]
    pub prefix_items: Vec<Schema>,
    #[serde(rename = "minItems", default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    #[serde(rename = "maxItems", default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    #[serde(
        rename = "uniqueItems",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unique_items: Option<bool>,

    // ---- string ----
    #[serde(rename = "minLength", default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    #[serde(rename = "maxLength", default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    // ---- number ----
    // Serialised through `number` so a whole value reads as `13` rather than
    // `13.0`; both are valid JSON Schema, but only one looks hand-written.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "number::serialize"
    )]
    pub minimum: Option<f64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "number::serialize"
    )]
    pub maximum: Option<f64>,
    #[serde(
        rename = "exclusiveMinimum",
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "number::serialize"
    )]
    pub exclusive_minimum: Option<f64>,
    #[serde(
        rename = "exclusiveMaximum",
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "number::serialize"
    )]
    pub exclusive_maximum: Option<f64>,
    #[serde(
        rename = "multipleOf",
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "number::serialize"
    )]
    pub multiple_of: Option<f64>,

    // ---- enum / const ----
    #[serde(rename = "enum", default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<Value>,
    #[serde(rename = "const", default, skip_serializing_if = "Option::is_none")]
    pub const_value: Option<Value>,

    // ---- composition ----
    #[serde(rename = "oneOf", default, skip_serializing_if = "Vec::is_empty")]
    pub one_of: Vec<Schema>,
    #[serde(rename = "anyOf", default, skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<Schema>,
    #[serde(rename = "allOf", default, skip_serializing_if = "Vec::is_empty")]
    pub all_of: Vec<Schema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not: Option<Box<Schema>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<Discriminator>,

    /// `x-` prefixed vendor extensions, flattened into the emitted object.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<Value>,
}

impl Schema {
    /// A schema that permits any value.
    pub fn any() -> Self {
        Self::default()
    }

    /// `{"$ref": "..."}`.
    pub fn reference(target: impl Into<String>) -> Self {
        Schema {
            reference: Some(target.into()),
            ..Default::default()
        }
    }

    /// A bare `{"type": "..."}` schema.
    pub fn of_type(ty: impl Into<SchemaType>) -> Self {
        Schema {
            schema_type: Some(ty.into()),
            ..Default::default()
        }
    }

    /// A `{"type": "...", "format": "..."}` schema.
    pub fn typed(ty: &str, format: &str) -> Self {
        Schema {
            schema_type: Some(ty.into()),
            format: Some(format.to_owned()),
            ..Default::default()
        }
    }

    /// An array schema whose items match `items`.
    pub fn array(items: Schema) -> Self {
        Schema {
            schema_type: Some("array".into()),
            items: Some(Box::new(items)),
            ..Default::default()
        }
    }

    /// An object schema whose values match `values`.
    pub fn map_of(values: Schema) -> Self {
        Schema {
            schema_type: Some("object".into()),
            additional_properties: Some(AdditionalProperties::Schema(Box::new(values))),
            ..Default::default()
        }
    }

    /// True when this schema is nothing but a `$ref`.
    ///
    /// Such a schema cannot carry sibling annotations in strict 3.0 tooling, so
    /// callers that need to attach a description wrap it in `allOf` instead.
    pub fn is_pure_ref(&self) -> bool {
        self.reference.is_some()
    }

    /// Marks the schema as accepting `null` in addition to its current type.
    pub fn nullable(mut self) -> Self {
        if self.reference.is_some() || !self.one_of.is_empty() || !self.any_of.is_empty() {
            // A `$ref` (or composition) can't grow a `type` keyword, so express
            // nullability as a union instead.
            let inner = std::mem::take(&mut self);
            return Schema {
                any_of: vec![inner, Schema::of_type("null")],
                ..Default::default()
            };
        }
        self.schema_type = Some(match self.schema_type.take() {
            Some(t) => t.or_null(),
            None => return self,
        });
        self
    }

    /// Attaches a description, wrapping in `allOf` when the schema is a bare
    /// `$ref` so the annotation is not silently dropped by consumers.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        if self.is_pure_ref() {
            let inner = std::mem::take(&mut self);
            return Schema {
                all_of: vec![inner],
                description: Some(description.into()),
                ..Default::default()
            };
        }
        self.description = Some(description.into());
        self
    }

    /// Adds an example value.
    pub fn with_example(mut self, example: Value) -> Self {
        self.examples.push(example);
        self
    }

    /// Sets a vendor extension. Keys are prefixed with `x-` when missing.
    pub fn with_extension(mut self, key: impl Into<String>, value: Value) -> Self {
        let key = key.into();
        let key = if key.starts_with("x-") {
            key
        } else {
            format!("x-{key}")
        };
        self.extensions.insert(key, value);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_schema_serialises_to_empty_object() {
        assert_eq!(serde_json::to_string(&Schema::any()).unwrap(), "{}");
    }

    #[test]
    fn nullable_promotes_type_to_union() {
        let s = Schema::of_type("string").nullable();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["type"], serde_json::json!(["string", "null"]));
    }

    #[test]
    fn nullable_ref_becomes_any_of() {
        let s = Schema::reference("#/components/schemas/User").nullable();
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("type").is_none());
        assert_eq!(v["anyOf"][0]["$ref"], "#/components/schemas/User");
        assert_eq!(v["anyOf"][1]["type"], "null");
    }

    #[test]
    fn description_on_ref_is_preserved_via_all_of() {
        let s = Schema::reference("#/components/schemas/User").with_description("the owner");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["description"], "the owner");
        assert_eq!(v["allOf"][0]["$ref"], "#/components/schemas/User");
    }

    #[test]
    fn extensions_are_flattened_and_prefixed() {
        let s = Schema::of_type("string").with_extension("internal", serde_json::json!(true));
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["x-internal"], true);
    }
}
