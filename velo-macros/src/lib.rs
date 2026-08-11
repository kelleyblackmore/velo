//! Procedural macros for [`velo`](https://docs.rs/velo).
//!
//! These are re-exported from `velo` itself; depend on that crate rather than
//! this one.

use proc_macro::TokenStream;
use syn::DeriveInput;

mod attr;
mod route;
mod schema;

/// Derives JSON Schema generation and validation for a type.
///
/// One derive gives you three things that otherwise drift apart: the schema
/// published in `/openapi.json`, the runtime checks applied to incoming
/// bodies, and the documentation shown in the UI.
///
/// # Container options
///
/// * `#[schema(rename = "Name")]` — the component name.
/// * `#[schema(inline)]` — inline the schema instead of registering a
///   component. Use for wrappers you do not want cluttering the document.
/// * `#[schema(title = "...")]`, `#[schema(description = "...")]` — prose. A
///   doc comment supplies the description when this is absent.
/// * `#[schema(example = "...")]` — a JSON string or any `Serialize` value.
/// * `#[schema(deny_unknown_fields)]` — `additionalProperties: false`.
///
/// `serde`'s `rename`, `rename_all`, `deny_unknown_fields`, `transparent`,
/// `tag`, `content`, and `untagged` are all honoured, so the schema matches
/// what serde actually emits.
///
/// # Field options
///
/// * `#[schema(description = "...", example = ..., default = ..., read_only,
///   write_only, deprecated, rename = "...")]`
/// * `#[validate(...)]` — see below.
///
/// # Validation
///
/// Every rule contributes both a schema keyword and a runtime check:
///
/// | Rule | Applies to | Schema keyword |
/// |---|---|---|
/// | `min_length` / `max_length` | strings, collections | `minLength` / `maxLength` |
/// | `min_items` / `max_items` | collections | `minItems` / `maxItems` |
/// | `minimum` / `maximum` | numbers | `minimum` / `maximum` |
/// | `exclusive_minimum` / `exclusive_maximum` | numbers | `exclusiveMinimum` / `exclusiveMaximum` |
/// | `multiple_of` | numbers | `multipleOf` |
/// | `range(min = .., max = ..)` | numbers | `minimum` / `maximum` |
/// | `pattern = "regex"` | strings | `pattern` |
/// | `format = "email"` | strings | `format` |
/// | `non_blank` | strings | `minLength: 1` |
/// | `nested` | any `Validate` type | — |
/// | `custom = "path::to::fn"` | any | — |
///
/// A rule applied to an `Option<T>` field checks the value only when it is
/// present.
///
/// ```ignore
/// #[derive(Schema, serde::Deserialize)]
/// #[serde(rename_all = "camelCase")]
/// struct CreateUser {
///     /// The user's display name.
///     #[validate(min_length = 1, max_length = 64, non_blank)]
///     display_name: String,
///
///     #[validate(format = "email")]
///     email: String,
///
///     #[validate(minimum = 13, maximum = 130)]
///     age: Option<u8>,
/// }
/// ```
#[proc_macro_derive(Schema, attributes(schema, validate))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    schema::derive(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

macro_rules! method_macro {
    ($name:ident, $method:literal, $doc:literal) => {
        #[doc = $doc]
        ///
        /// The first argument is the route template; `{name}` captures one
        /// segment and `{*name}` captures the rest of the path.
        ///
        /// Optional settings: `tags = ["a", "b"]`, `summary = "..."`,
        /// `description = "..."`, `operation_id = "..."`, `deprecated`,
        /// `hidden`.
        ///
        /// The handler keeps its own name and stays directly callable; mount it
        /// with [`routes!`].
        #[proc_macro_attribute]
        pub fn $name(attr: TokenStream, item: TokenStream) -> TokenStream {
            route::expand($method, attr.into(), item.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into()
        }
    };
}

method_macro!(get, "GET", "Registers the handler as a `GET` route.");
method_macro!(post, "POST", "Registers the handler as a `POST` route.");
method_macro!(put, "PUT", "Registers the handler as a `PUT` route.");
method_macro!(patch, "PATCH", "Registers the handler as a `PATCH` route.");
method_macro!(
    delete,
    "DELETE",
    "Registers the handler as a `DELETE` route."
);
method_macro!(head, "HEAD", "Registers the handler as a `HEAD` route.");
method_macro!(
    options,
    "OPTIONS",
    "Registers the handler as an `OPTIONS` route."
);

/// Collects handlers into a list of routes.
///
/// ```ignore
/// App::new().mount(routes![list_users, create_user, users::get_user]);
/// ```
///
/// Each name refers to a function annotated with one of the method attributes.
#[proc_macro]
pub fn routes(input: TokenStream) -> TokenStream {
    route::routes(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
