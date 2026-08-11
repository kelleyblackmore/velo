//! OpenAPI 3.1 document model and JSON Schema 2020-12 generation.
//!
//! This crate is deliberately independent of the HTTP layer: it knows how to
//! *describe* an API, not how to serve one. [`velo`](https://docs.rs/velo) wires
//! it to real handlers so that a route's Rust signature is the single source of
//! truth for both runtime behaviour and the published document.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod gen;
mod impls;
mod schema;
mod spec;

pub use gen::{name_of, schema_for, JsonSchema, SchemaGenerator};
pub use schema::{AdditionalProperties, Discriminator, Schema, SchemaType};
pub use spec::*;

/// Convenience alias for the ordered maps used throughout the document model.
pub type Map<V> = indexmap::IndexMap<String, V>;

/// Re-exported so downstream derives can build literal values without taking
/// their own `serde_json` dependency.
pub use serde_json::{json, Value};
