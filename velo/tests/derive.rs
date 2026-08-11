//! What `#[derive(Schema)]` produces.
//!
//! These assert on the emitted document rather than on the macro's tokens,
//! because the document is the contract users actually depend on.

use serde::{Deserialize, Serialize};
use velo::openapi::{schema_for, Schema as OpenApiSchema};
use velo::{JsonSchema, Schema, Validate};

fn schema<T: JsonSchema>() -> serde_json::Value {
    let (root, definitions) = schema_for::<T>();
    // Resolve the root `$ref` so tests read against the real object.
    let resolved = root
        .reference
        .as_ref()
        .and_then(|r| r.strip_prefix("#/components/schemas/"))
        .and_then(|name| definitions.get(name).cloned())
        .unwrap_or(root);
    serde_json::to_value(resolved).unwrap()
}

fn definitions<T: JsonSchema>() -> Vec<String> {
    schema_for::<T>().1.keys().cloned().collect()
}

// ---------------------------------------------------------------------------
// structs
// ---------------------------------------------------------------------------

/// A person.
///
/// The second paragraph becomes the description.
#[derive(Debug, Schema, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Person {
    /// What to call them.
    display_name: String,
    /// Absent when unknown.
    age: Option<u8>,
    #[serde(default)]
    nickname: String,
    #[serde(skip)]
    #[allow(dead_code)]
    internal_notes: String,
}

#[test]
fn field_names_follow_the_serde_rename_rule() {
    let schema = schema::<Person>();
    assert!(schema["properties"].get("displayName").is_some());
    assert!(schema["properties"].get("display_name").is_none());
}

#[test]
fn skipped_fields_are_absent_from_the_schema() {
    let schema = schema::<Person>();
    assert!(schema["properties"].get("internalNotes").is_none());
}

#[test]
fn options_are_nullable_and_not_required() {
    let schema = schema::<Person>();
    assert_eq!(
        schema["properties"]["age"]["type"],
        serde_json::json!(["integer", "null"])
    );
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, vec!["displayName"]);
}

#[test]
fn serde_default_makes_a_field_optional() {
    let required = schema::<Person>()["required"].to_string();
    assert!(!required.contains("nickname"));
}

#[test]
fn doc_comments_become_descriptions() {
    let schema = schema::<Person>();
    assert_eq!(
        schema["properties"]["displayName"]["description"],
        "What to call them."
    );
    assert!(schema["description"]
        .as_str()
        .unwrap()
        .contains("second paragraph"));
}

#[test]
fn deny_unknown_fields_reaches_the_schema() {
    assert_eq!(schema::<Person>()["additionalProperties"], false);
}

// ---------------------------------------------------------------------------
// validation
// ---------------------------------------------------------------------------

#[derive(Debug, Schema, Deserialize)]
struct Signup {
    #[validate(min_length = 3, max_length = 20, pattern = "^[a-z0-9_]+$")]
    username: String,
    #[validate(format = "email")]
    email: String,
    #[validate(range(min = 13, max = 130))]
    age: Option<u8>,
    #[validate(min_items = 1, max_items = 3)]
    interests: Vec<String>,
}

fn signup(username: &str, email: &str, age: Option<u8>, interests: &[&str]) -> Signup {
    Signup {
        username: username.into(),
        email: email.into(),
        age,
        interests: interests.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn validation_rules_become_schema_keywords() {
    let schema = schema::<Signup>();
    assert_eq!(schema["properties"]["username"]["minLength"], 3);
    assert_eq!(schema["properties"]["username"]["maxLength"], 20);
    assert_eq!(schema["properties"]["username"]["pattern"], "^[a-z0-9_]+$");
    assert_eq!(schema["properties"]["email"]["format"], "email");
    assert_eq!(schema["properties"]["age"]["minimum"], 13.0);
    assert_eq!(schema["properties"]["interests"]["minItems"], 1);
}

#[test]
fn a_valid_value_passes() {
    assert!(signup("ada", "ada@example.com", Some(36), &["maths"])
        .validate()
        .is_ok());
}

#[test]
fn every_broken_field_is_reported_at_once() {
    let errors = signup("A", "nope", Some(3), &[]).validate().unwrap_err();
    let pointers: Vec<&str> = errors
        .as_slice()
        .iter()
        .map(|e| e.pointer.as_str())
        .collect();

    // `username` breaks two rules, so it appears twice.
    assert!(pointers.contains(&"/username"));
    assert!(pointers.contains(&"/email"));
    assert!(pointers.contains(&"/age"));
    assert!(pointers.contains(&"/interests"));
}

#[test]
fn a_pattern_is_enforced_at_runtime_not_just_documented() {
    let errors = signup("Has Spaces", "a@b.co", None, &["x"])
        .validate()
        .unwrap_err();
    assert!(errors
        .as_slice()
        .iter()
        .any(|e| e.code == "pattern" && e.pointer == "/username"));
}

#[test]
fn an_absent_option_skips_its_rules() {
    // `age = None` must not trip the `minimum` rule.
    assert!(signup("ada", "a@b.co", None, &["x"]).validate().is_ok());
}

#[derive(Debug, Schema, Deserialize)]
struct Address {
    #[validate(non_blank)]
    street: String,
}

#[derive(Debug, Schema, Deserialize)]
struct Order {
    #[validate(nested)]
    shipping: Address,
    #[validate(nested)]
    billing: Option<Address>,
}

#[test]
fn nested_validation_reports_a_full_pointer() {
    let order = Order {
        shipping: Address {
            street: "  ".into(),
        },
        billing: None,
    };
    let errors = order.validate().unwrap_err();
    assert_eq!(errors.as_slice()[0].pointer, "/shipping/street");
}

#[test]
fn nested_validation_reaches_into_an_option() {
    let order = Order {
        shipping: Address {
            street: "Main St".into(),
        },
        billing: Some(Address { street: "".into() }),
    };
    let errors = order.validate().unwrap_err();
    assert_eq!(errors.as_slice()[0].pointer, "/billing/street");
}

// ---------------------------------------------------------------------------
// references, generics, newtypes
// ---------------------------------------------------------------------------

#[derive(Debug, Schema, Serialize)]
struct Item {
    sku: String,
}

#[derive(Debug, Schema, Serialize)]
struct Basket {
    items: Vec<Item>,
    featured: Item,
}

#[test]
fn nested_types_are_referenced_not_duplicated() {
    let schema = schema::<Basket>();
    assert_eq!(
        schema["properties"]["featured"]["$ref"],
        "#/components/schemas/Item"
    );
    assert_eq!(
        schema["properties"]["items"]["items"]["$ref"],
        "#/components/schemas/Item"
    );
    let names = definitions::<Basket>();
    assert_eq!(names.iter().filter(|n| *n == "Item").count(), 1);
}

#[derive(Debug, Schema, Serialize)]
#[allow(dead_code)]
struct Page<T> {
    items: Vec<T>,
    total: usize,
}

#[test]
fn generic_instantiations_get_distinct_component_names() {
    assert!(definitions::<Page<Item>>().contains(&"Page_Item".to_owned()));
    assert!(definitions::<Page<String>>().contains(&"Page_String".to_owned()));
}

#[derive(Debug, Schema, Serialize)]
#[allow(dead_code)]
struct UserId(u64);

#[test]
fn a_newtype_documents_as_its_inner_type() {
    let schema = schema::<UserId>();
    assert_eq!(schema["type"], "integer");
    assert!(schema.get("properties").is_none());
}

#[derive(Debug, Schema, Serialize)]
struct Meta {
    trace_id: String,
}

#[derive(Debug, Schema, Serialize)]
struct Envelope {
    #[serde(flatten)]
    meta: Meta,
    payload: String,
}

#[test]
fn flattened_fields_merge_into_the_parent_object() {
    let schema = schema::<Envelope>();
    assert!(schema["properties"].get("trace_id").is_some());
    assert!(schema["properties"].get("meta").is_none());
    assert!(schema["required"].to_string().contains("trace_id"));
}

#[derive(Debug, Schema, Serialize)]
struct Node {
    value: u32,
    children: Vec<Node>,
}

#[test]
fn a_self_referential_type_does_not_recurse_forever() {
    let schema = schema::<Node>();
    assert_eq!(
        schema["properties"]["children"]["items"]["$ref"],
        "#/components/schemas/Node"
    );
}

// ---------------------------------------------------------------------------
// enums
// ---------------------------------------------------------------------------

/// How an order is progressing.
#[derive(Debug, Schema, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Status {
    /// Waiting to be picked.
    Pending,
    Shipped,
    #[serde(rename = "CANCELLED_BY_USER")]
    Cancelled,
}

#[test]
fn a_unit_enum_is_a_string_with_a_closed_value_set() {
    let schema = schema::<Status>();
    assert_eq!(schema["type"], "string");
    assert_eq!(
        schema["enum"],
        serde_json::json!(["PENDING", "SHIPPED", "CANCELLED_BY_USER"])
    );
}

#[test]
fn variant_doc_comments_survive_as_an_extension() {
    let schema = schema::<Status>();
    assert_eq!(schema["x-enum-descriptions"][0], "Waiting to be picked.");
}

#[derive(Debug, Schema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(dead_code)]
enum Event {
    Created { id: u64 },
    Deleted { id: u64, reason: String },
}

#[test]
fn an_internally_tagged_enum_gets_a_discriminator() {
    let schema = schema::<Event>();
    assert_eq!(schema["discriminator"]["propertyName"], "kind");
    assert_eq!(schema["oneOf"].as_array().unwrap().len(), 2);
    assert_eq!(
        schema["oneOf"][0]["allOf"][0]["properties"]["kind"]["const"],
        "created"
    );
    assert_eq!(
        schema["oneOf"][1]["allOf"][1]["properties"]["reason"]["type"],
        "string"
    );
}

#[derive(Debug, Schema, Serialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum Id {
    Number(u64),
    Text(String),
}

#[test]
fn an_untagged_enum_is_an_any_of() {
    let schema = schema::<Id>();
    assert_eq!(schema["anyOf"][0]["type"], "integer");
    assert_eq!(schema["anyOf"][1]["type"], "string");
    assert!(schema.get("oneOf").is_none());
}

#[derive(Debug, Schema, Serialize)]
#[allow(dead_code)]
enum Shape {
    Circle(f64),
    Rect { w: f64, h: f64 },
}

#[test]
fn an_externally_tagged_enum_wraps_each_payload_in_its_name() {
    let schema = schema::<Shape>();
    assert_eq!(schema["oneOf"][0]["properties"]["Circle"]["type"], "number");
    assert_eq!(
        schema["oneOf"][1]["properties"]["Rect"]["properties"]["w"]["type"],
        "number"
    );
}

// ---------------------------------------------------------------------------
// annotations
// ---------------------------------------------------------------------------

#[derive(Debug, Schema, Serialize)]
#[schema(rename = "PublicProfile", title = "Public profile")]
struct Profile {
    #[schema(read_only, example = 42)]
    id: u64,
    #[schema(write_only)]
    password: String,
    #[schema(deprecated, description = "Use `handle` instead.")]
    username: String,
}

#[test]
fn container_rename_changes_the_component_name() {
    assert!(definitions::<Profile>().contains(&"PublicProfile".to_owned()));
}

#[test]
fn field_annotations_reach_the_schema() {
    let schema = schema::<Profile>();
    assert_eq!(schema["title"], "Public profile");
    assert_eq!(schema["properties"]["id"]["readOnly"], true);
    assert_eq!(schema["properties"]["id"]["examples"][0], 42);
    assert_eq!(schema["properties"]["password"]["writeOnly"], true);
    assert_eq!(schema["properties"]["username"]["deprecated"], true);
    assert_eq!(
        schema["properties"]["username"]["description"],
        "Use `handle` instead."
    );
}

#[derive(Debug, Schema, Serialize)]
#[schema(inline)]
struct Inlined {
    value: u32,
}

#[derive(Debug, Schema, Serialize)]
struct HasInlined {
    inner: Inlined,
}

#[test]
fn an_inline_type_registers_no_component() {
    let names = definitions::<HasInlined>();
    assert!(!names.contains(&"Inlined".to_owned()));
    assert_eq!(
        schema::<HasInlined>()["properties"]["inner"]["type"],
        "object"
    );
}

#[derive(Debug, Schema, Serialize)]
struct Annotated {
    /// The item this line refers to.
    #[schema(deprecated)]
    tag: Item,
    plain: Item,
}

#[test]
fn annotations_on_a_referenced_type_wrap_it_rather_than_being_dropped() {
    // A bare `$ref` cannot carry sibling keywords, so an annotated reference
    // is wrapped in `allOf` and the annotation goes on the wrapper.
    let schema = schema::<Annotated>();
    let tag = &schema["properties"]["tag"];
    assert_eq!(tag["allOf"][0]["$ref"], "#/components/schemas/Item");
    assert_eq!(tag["description"], "The item this line refers to.");
    assert_eq!(tag["deprecated"], true);

    // An unannotated reference stays a plain `$ref`, with no wrapper noise.
    assert_eq!(
        schema["properties"]["plain"],
        serde_json::json!({ "$ref": "#/components/schemas/Item" })
    );
}

#[test]
fn schemas_are_valid_json_schema_documents() {
    // A round-trip proves the emitted keywords all deserialise back, which
    // catches accidental shape changes in the model.
    let (_, definitions) = schema_for::<Basket>();
    for (name, schema) in &definitions {
        let json = serde_json::to_string(schema).unwrap();
        let parsed: OpenApiSchema = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("`{name}` did not round-trip: {e}"));
        assert_eq!(&parsed, schema);
    }
}
