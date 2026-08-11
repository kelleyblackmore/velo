//! The OpenAPI 3.1 document model.

use crate::schema::Schema;
use crate::Map;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A complete OpenAPI 3.1 document.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenApi {
    pub openapi: String,
    pub info: Info,
    #[serde(rename = "jsonSchemaDialect", skip_serializing_if = "Option::is_none")]
    pub json_schema_dialect: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<Server>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub paths: Map<PathItem>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub webhooks: Map<PathItem>,
    #[serde(skip_serializing_if = "Components::is_empty")]
    pub components: Components,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security: Vec<SecurityRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    #[serde(rename = "externalDocs", skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocs>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<Value>,
}

impl Default for OpenApi {
    fn default() -> Self {
        Self {
            openapi: "3.1.0".into(),
            info: Info::default(),
            json_schema_dialect: None,
            servers: Vec::new(),
            paths: Map::new(),
            webhooks: Map::new(),
            components: Components::default(),
            security: Vec::new(),
            tags: Vec::new(),
            external_docs: None,
            extensions: Map::new(),
        }
    }
}

/// Document metadata.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Info {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "termsOfService", skip_serializing_if = "Option::is_none")]
    pub terms_of_service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<Contact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
    pub version: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Contact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct License {
    pub name: String,
    /// SPDX expression; mutually exclusive with `url` per the 3.1 spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Server {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub variables: Map<ServerVariable>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerVariable {
    #[serde(rename = "enum", default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
    pub default: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "externalDocs", skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocs>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExternalDocs {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The operations available on a single path.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PathItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub put: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<Operation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Operation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<Server>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
}

impl PathItem {
    /// Mutable access to the slot for an HTTP method, if 3.1 defines one.
    pub fn slot(&mut self, method: &str) -> Option<&mut Option<Operation>> {
        Some(match method.to_ascii_uppercase().as_str() {
            "GET" => &mut self.get,
            "PUT" => &mut self.put,
            "POST" => &mut self.post,
            "DELETE" => &mut self.delete,
            "OPTIONS" => &mut self.options,
            "HEAD" => &mut self.head,
            "PATCH" => &mut self.patch,
            "TRACE" => &mut self.trace,
            _ => return None,
        })
    }
}

/// A single API operation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Operation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "externalDocs", skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocs>,
    #[serde(rename = "operationId", skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    #[serde(rename = "requestBody", skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub responses: Map<Response>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub callbacks: Map<Map<PathItem>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    /// `None` inherits the document-level requirement; `Some(vec![])` opts out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<SecurityRequirement>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<Server>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extensions: Map<Value>,
}

/// Where a parameter is carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterIn {
    Query,
    Header,
    Path,
    Cookie,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: ParameterIn,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub examples: Map<Example>,
}

impl Parameter {
    pub fn new(name: impl Into<String>, location: ParameterIn, schema: Schema) -> Self {
        Self {
            name: name.into(),
            // Path parameters are required by definition; the constructor
            // encodes that so callers cannot emit an invalid document.
            required: location == ParameterIn::Path,
            location,
            description: None,
            deprecated: false,
            style: None,
            explode: None,
            schema: Some(schema),
            examples: Map::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Example {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub content: Map<MediaType>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MediaType {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub examples: Map<Example>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub encoding: Map<Encoding>,
}

impl MediaType {
    pub fn new(schema: Schema) -> Self {
        Self {
            schema: Some(schema),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Encoding {
    #[serde(rename = "contentType", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explode: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub description: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub headers: Map<Header>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub content: Map<MediaType>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub links: Map<Link>,
}

impl Response {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            ..Default::default()
        }
    }

    pub fn with_json(mut self, schema: Schema) -> Self {
        self.content
            .insert("application/json".into(), MediaType::new(schema));
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Header {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Link {
    #[serde(rename = "operationId", skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub parameters: Map<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A map from security scheme name to the scopes it must grant.
pub type SecurityRequirement = Map<Vec<String>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Components {
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub schemas: Map<Schema>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub responses: Map<Response>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub parameters: Map<Parameter>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub examples: Map<Example>,
    #[serde(
        rename = "requestBodies",
        default,
        skip_serializing_if = "Map::is_empty"
    )]
    pub request_bodies: Map<RequestBody>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub headers: Map<Header>,
    #[serde(
        rename = "securitySchemes",
        default,
        skip_serializing_if = "Map::is_empty"
    )]
    pub security_schemes: Map<SecurityScheme>,
}

impl Components {
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
            && self.responses.is_empty()
            && self.parameters.is_empty()
            && self.examples.is_empty()
            && self.request_bodies.is_empty()
            && self.headers.is_empty()
            && self.security_schemes.is_empty()
    }
}

/// An authentication scheme.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SecurityScheme {
    #[serde(rename = "apiKey")]
    ApiKey {
        name: String,
        #[serde(rename = "in")]
        location: ApiKeyLocation,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(rename = "http")]
    Http {
        scheme: String,
        #[serde(rename = "bearerFormat", skip_serializing_if = "Option::is_none")]
        bearer_format: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(rename = "oauth2")]
    OAuth2 {
        flows: Box<OAuthFlows>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(rename = "openIdConnect")]
    OpenIdConnect {
        #[serde(rename = "openIdConnectUrl")]
        open_id_connect_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(rename = "mutualTLS")]
    MutualTls {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl SecurityScheme {
    /// `Authorization: Bearer <jwt>`.
    pub fn bearer_jwt() -> Self {
        SecurityScheme::Http {
            scheme: "bearer".into(),
            bearer_format: Some("JWT".into()),
            description: None,
        }
    }

    /// An API key carried in a header.
    pub fn api_key_header(name: impl Into<String>) -> Self {
        SecurityScheme::ApiKey {
            name: name.into(),
            location: ApiKeyLocation::Header,
            description: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyLocation {
    Query,
    Header,
    Cookie,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OAuthFlows {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implicit: Option<OAuthFlow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<OAuthFlow>,
    #[serde(rename = "clientCredentials", skip_serializing_if = "Option::is_none")]
    pub client_credentials: Option<OAuthFlow>,
    #[serde(rename = "authorizationCode", skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<OAuthFlow>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OAuthFlow {
    #[serde(rename = "authorizationUrl", skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(rename = "tokenUrl", skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(rename = "refreshUrl", skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    #[serde(default)]
    pub scopes: Map<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_document_omits_empty_sections() {
        let doc = OpenApi {
            info: Info {
                title: "Demo".into(),
                version: "1.0.0".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["openapi"], "3.1.0");
        assert!(v.get("components").is_none());
        assert!(v.get("paths").is_none());
    }

    #[test]
    fn path_parameters_are_required_by_construction() {
        let p = Parameter::new("id", ParameterIn::Path, Schema::of_type("string"));
        assert!(p.required);
        let q = Parameter::new("page", ParameterIn::Query, Schema::of_type("integer"));
        assert!(!q.required);
    }

    #[test]
    fn security_scheme_tags_its_type() {
        let v = serde_json::to_value(SecurityScheme::bearer_jwt()).unwrap();
        assert_eq!(v["type"], "http");
        assert_eq!(v["scheme"], "bearer");
        assert_eq!(v["bearerFormat"], "JWT");
    }
}
