//! # velo
//!
//! An async Rust web framework with first-class OpenAPI 3.1.
//!
//! The premise is FastAPI's: a handler's signature already contains everything
//! needed to describe the endpoint, so writing that description twice is a bug
//! waiting to happen. `velo` reads the signature and generates the document
//! from it — no annotation duplicating a type, no separate spec file to keep
//! in step.
//!
//! ```no_run
//! use velo::prelude::*;
//!
//! #[derive(Schema, serde::Deserialize)]
//! struct NewUser {
//!     #[validate(min_length = 1, max_length = 64)]
//!     name: String,
//!     #[validate(format = "email")]
//!     email: String,
//! }
//!
//! #[derive(Schema, serde::Serialize)]
//! struct User {
//!     id: u64,
//!     name: String,
//! }
//!
//! /// Create a user.
//! #[post("/users", tags = ["users"])]
//! async fn create_user(Json(body): Json<NewUser>) -> Result<Created<Json<User>>, ApiError> {
//!     Ok(Created::at("/users/1", Json(User { id: 1, name: body.name })))
//! }
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     App::new()
//!         .title("Users API")
//!         .version("1.0.0")
//!         .mount(routes![create_user])
//!         .serve(([127, 0, 0, 1], 8080))
//!         .await
//! }
//! ```
//!
//! That program serves the endpoint, `/openapi.json`, and a browsable UI at
//! `/docs`.
//!
//! ## What differs from FastAPI
//!
//! * **Validation is checked at compile time.** A `#[validate(min_length)]` on
//!   a number will not build, rather than failing at first request.
//! * **Errors are RFC 9457 problem details**, described in the document, not
//!   an ad-hoc `{"detail": ...}` shape.
//! * **Dependencies are memoised per request** and can declare their own
//!   security requirements, so an auth dependency shows up in the docs.
//! * **No interpreter.** Handlers are monomorphised; extraction is a
//!   deserialise, not a reflection pass.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod app;
pub mod body;
pub mod docs;
pub mod error;
pub mod extract;
pub mod middleware;
pub mod operation;
pub mod request;
pub mod response;
pub mod route;
pub mod router;
pub mod sse;
pub mod state;
pub mod testing;
pub mod validate;

pub use app::{App, Bound, Router, Service};
pub use body::ResBody;
pub use docs::{Docs, Renderer};
pub use error::{ApiError, FieldError, ProblemDetails};
pub use extract::{
    Bearer, Cookies, Depends, Extension, Form, FromRequest, Json, Multipart, Part, Path, Query,
    State,
};
pub use operation::{OperationContext, OperationInput, OperationOutput};
pub use request::Request;
pub use response::{
    Accepted, Created, Html, IntoResponse, NoContent, Permanent, Redirect, Response, SeeOther,
    Temporary, WithHeaders, WithStatus,
};
pub use route::{IntoRoute, RouteDef};
pub use sse::{Event, Sse};
pub use validate::{Validate, ValidationErrors};

/// The `http` crate, re-exported.
///
/// Handlers routinely need `HeaderMap`, `HeaderName`, `HeaderValue`, and the
/// `header::*` constants. Re-exporting them means a consumer cannot end up
/// with a second, incompatible version of the very types this crate hands it.
pub use http;

/// The OpenAPI document model, re-exported so downstream crates need not
/// depend on it separately.
pub use velo_openapi as openapi;
pub use velo_openapi::{JsonSchema, SchemaGenerator};

#[cfg(feature = "macros")]
pub use velo_macros::{delete, get, head, options, patch, post, put, routes, Schema};

/// Everything a typical application needs.
pub mod prelude {
    pub use crate::app::{App, Router};
    pub use crate::error::{ApiError, FieldError};
    pub use crate::extract::{
        Bearer, Cookies, Dependency, Depends, Extension, Form, FromRequest, Json, Multipart, Part,
        Path, Query, State,
    };
    pub use crate::request::Request;
    pub use crate::response::{
        Accepted, Created, Html, IntoResponse, NoContent, Permanent, Redirect, Response, SeeOther,
        Temporary, WithStatus,
    };
    pub use crate::sse::{Event, Sse};
    pub use crate::testing::{TestClient, TestRequest};
    pub use crate::validate::{Validate, ValidationErrors};
    pub use crate::{Docs, Renderer};
    pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
    pub use velo_openapi::JsonSchema;

    #[cfg(feature = "macros")]
    pub use velo_macros::{delete, get, head, options, patch, post, put, routes, Schema};
}

/// Re-exported for generated code. Not a stable API.
#[doc(hidden)]
pub mod __private {
    pub use crate::extract::FromRequest;
    pub use crate::operation::{OperationContext, OperationInput, OperationOutput};
    pub use crate::request::Request;
    pub use crate::response::{IntoResponse, Response};
    pub use crate::route::{BoxFuture, IntoRoute, RouteDef};
    pub use crate::validate::{rules, Validate, ValidationErrors};
    pub use std::boxed::Box;
    pub use std::string::String;
    pub use std::sync::{Arc, OnceLock};
    pub use velo_openapi::{
        name_of, AdditionalProperties, Discriminator, JsonSchema, Schema, SchemaGenerator,
    };

    #[cfg(feature = "regex")]
    pub use regex::Regex;

    pub use serde_json::Value;

    /// `#[schema(example = "...")]` with a JSON-looking string.
    pub fn json_from_str(raw: &str) -> Value {
        // Falling back to a string keeps a malformed example from failing the
        // build; the example is documentation, not a contract.
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
    }

    pub fn json_string(value: &str) -> Value {
        Value::String(value.to_owned())
    }

    pub fn json_bool(value: bool) -> Value {
        Value::Bool(value)
    }

    pub fn json_null() -> Value {
        Value::Null
    }

    pub fn json_array(values: Vec<Value>) -> Value {
        Value::Array(values)
    }

    pub fn json_value<T: serde::Serialize + ?Sized>(value: &T) -> Value {
        serde_json::to_value(value).unwrap_or(Value::Null)
    }

    /// A schema matching exactly one string, used for enum discriminants.
    pub fn const_string(value: &str) -> Schema {
        Schema {
            const_value: Some(Value::String(value.to_owned())),
            ..Schema::of_type("string")
        }
    }
}
