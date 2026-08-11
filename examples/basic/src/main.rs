//! A small but complete `velo` service.
//!
//! Run it, then open <http://127.0.0.1:8080/docs>.
//!
//! Everything the documentation shows — parameters, bodies, validation
//! constraints, error shapes, the auth requirement on `DELETE` — is generated
//! from the handler signatures below. There is no spec file.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velo::middleware::{Cors, Logger, RequestId, Timeout};
use velo::prelude::*;
use velo::sse::EventStream;

// ---------------------------------------------------------------------------
// models
// ---------------------------------------------------------------------------

/// A user of the system.
#[derive(Clone, Debug, Schema, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    /// Server-assigned identifier.
    #[schema(read_only, example = 1)]
    pub id: u64,
    /// The name shown to other people.
    pub display_name: String,
    pub email: String,
    /// When absent, the user has not told us.
    pub age: Option<u8>,
    #[serde(default)]
    pub roles: Vec<Role>,
}

/// What a user is allowed to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Schema, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Full access.
    Admin,
    /// Can change their own data.
    Member,
    /// Read-only.
    Guest,
}

/// The payload for creating a user.
#[derive(Debug, Schema, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateUser {
    /// Shown to other people. Leading and trailing whitespace is not enough.
    #[validate(min_length = 1, max_length = 64, non_blank)]
    pub display_name: String,

    #[validate(format = "email", max_length = 254)]
    pub email: String,

    /// Optional, but if given it has to be plausible.
    #[validate(minimum = 13, maximum = 130)]
    pub age: Option<u8>,

    #[serde(default)]
    #[validate(max_items = 8)]
    pub roles: Vec<Role>,
}

/// Query parameters for listing users.
#[derive(Debug, Schema, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    /// Case-insensitive substring match against the display name.
    pub q: Option<String>,
    /// Repeat to filter on several roles: `?role=admin&role=member`.
    #[serde(default)]
    pub role: Vec<Role>,
    /// Maximum number of results.
    #[validate(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
}

/// A page of results.
#[derive(Debug, Schema, serde::Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

/// An in-memory store, standing in for a database.
#[derive(Debug, Default)]
pub struct Store {
    users: Mutex<BTreeMap<u64, User>>,
    next_id: AtomicU64,
}

impl Store {
    fn insert(&self, create: CreateUser) -> User {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let user = User {
            id,
            display_name: create.display_name,
            email: create.email,
            age: create.age,
            roles: create.roles,
        };
        self.users.lock().unwrap().insert(id, user.clone());
        user
    }

    fn get(&self, id: u64) -> Option<User> {
        self.users.lock().unwrap().get(&id).cloned()
    }

    fn remove(&self, id: u64) -> bool {
        self.users.lock().unwrap().remove(&id).is_some()
    }

    fn list(&self, query: &ListQuery) -> (Vec<User>, usize) {
        let users = self.users.lock().unwrap();
        let matched: Vec<User> = users
            .values()
            .filter(|user| match &query.q {
                Some(needle) => user
                    .display_name
                    .to_lowercase()
                    .contains(&needle.to_lowercase()),
                None => true,
            })
            .filter(|user| {
                query.role.is_empty() || user.roles.iter().any(|r| query.role.contains(r))
            })
            .cloned()
            .collect();

        let total = matched.len();
        let limit = query.limit.unwrap_or(20) as usize;
        (matched.into_iter().take(limit).collect(), total)
    }
}

/// The set of tokens that may delete things.
#[derive(Debug)]
pub struct AdminTokens(pub Vec<String>);

/// An authenticated administrator.
///
/// Taking this as a handler argument both enforces the check and puts the
/// security requirement into the document.
#[derive(Clone, Debug)]
pub struct Admin;

impl Dependency for Admin {
    async fn resolve(req: &mut Request) -> Result<Self, ApiError> {
        let Bearer(token) = Bearer::from_request(req).await?;
        let State(tokens) = State::<AdminTokens>::from_request(req).await?;

        if tokens.0.contains(&token) {
            Ok(Admin)
        } else {
            Err(ApiError::forbidden("That token is not an admin token."))
        }
    }

    fn describe(ctx: &mut velo::OperationContext<'_>) {
        // Inherit the bearer requirement and its 401.
        <Bearer as velo::OperationInput>::describe(ctx);
        ctx.add_response(
            403,
            velo::openapi::Response::new("The token is valid but not an admin token"),
        );
    }
}

// ---------------------------------------------------------------------------
// handlers
// ---------------------------------------------------------------------------

/// List users.
///
/// Supports substring search and filtering by role. Results are capped at
/// `limit`, which defaults to 20.
#[get("/users", tags = ["users"])]
async fn list_users(
    Query(query): Query<ListQuery>,
    State(store): State<Store>,
) -> Json<Page<User>> {
    let (items, total) = store.list(&query);
    Json(Page { items, total })
}

/// Fetch one user by id.
#[get("/users/{id}", tags = ["users"])]
async fn get_user(Path(id): Path<u64>, State(store): State<Store>) -> Result<Json<User>, ApiError> {
    store
        .get(id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("No user with id {id}.")))
}

/// Create a user.
///
/// The body is validated before the handler runs; a failure comes back as a
/// 422 listing every offending field.
#[post("/users", tags = ["users"])]
async fn create_user(
    Json(body): Json<CreateUser>,
    State(store): State<Store>,
) -> Created<Json<User>> {
    let user = store.insert(body);
    Created::at(format!("/users/{}", user.id), Json(user))
}

/// Delete a user. Requires an admin token.
#[delete("/users/{id}", tags = ["users"])]
async fn delete_user(
    Path(id): Path<u64>,
    _admin: Depends<Admin>,
    State(store): State<Store>,
) -> Result<NoContent, ApiError> {
    if store.remove(id) {
        Ok(NoContent)
    } else {
        Err(ApiError::not_found(format!("No user with id {id}.")))
    }
}

/// A liveness probe.
#[get("/health", tags = ["ops"])]
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Stream a countdown as server-sent events.
///
/// Open this in a browser to watch events arrive one per second. The return
/// type is `Sse<EventStream>` rather than `Sse<impl Stream>` because the
/// generated operation has to name the type.
#[get("/countdown/{from}", tags = ["ops"])]
async fn countdown(Path(from): Path<u8>) -> Sse<EventStream> {
    let stream = futures_util::stream::unfold(from, |remaining| async move {
        if remaining == 0 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        Some((
            Event::data(remaining.to_string()).named("tick"),
            remaining - 1,
        ))
    });
    Sse::from_stream(stream)
}

// ---------------------------------------------------------------------------
// wiring
// ---------------------------------------------------------------------------

fn app() -> App {
    App::new()
        .title("Users API")
        .version("1.0.0")
        .description(
            "A demonstration service. Every part of this document was generated \
             from the Rust handler signatures.",
        )
        .server("http://127.0.0.1:8080", Some("Local development"))
        .tag("users", "Creating, reading, and removing users.")
        .tag("ops", "Health and diagnostics.")
        .with_state(Store::default())
        .with_state(AdminTokens(vec!["admin-token".into()]))
        .layer(Arc::new(RequestId::new()))
        .layer(Arc::new(Logger::new()))
        .layer(Arc::new(Cors::permissive()))
        .layer(Arc::new(Timeout::seconds(30)))
        .mount_at(
            "/api/v1",
            routes![list_users, get_user, create_user, delete_user],
        )
        .mount(routes![health, countdown])
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = app();

    if std::env::args().any(|arg| arg == "--print-openapi") {
        let document = app.openapi();
        println!("{}", serde_json::to_string_pretty(&document).unwrap());
        return Ok(());
    }

    println!("docs:    http://127.0.0.1:8080/docs");
    println!("openapi: http://127.0.0.1:8080/openapi.json");
    app.serve(([127, 0, 0, 1], 8080)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use velo::prelude::StatusCode;

    fn client() -> TestClient {
        TestClient::new(app())
    }

    #[tokio::test]
    async fn a_created_user_can_be_read_back() {
        let client = client();

        let created = client
            .post_json(
                "/api/v1/users",
                r#"{"displayName":"Ada","email":"ada@example.com","age":36}"#,
            )
            .await;
        created.assert_status(StatusCode::CREATED);
        assert_eq!(created.header("location"), Some("/users/1"));

        let fetched = client.get("/api/v1/users/1").await;
        fetched.assert_status(StatusCode::OK);
        assert_eq!(fetched.json()["displayName"], "Ada");
    }

    #[tokio::test]
    async fn validation_failures_list_every_bad_field() {
        let response = client()
            .post_json(
                "/api/v1/users",
                r#"{"displayName":"   ","email":"not-an-email","age":3}"#,
            )
            .await;

        response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json();
        assert_eq!(body["status"], 422);
        assert_eq!(body["title"], "Validation failed");

        let pointers: Vec<&str> = body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["pointer"].as_str().unwrap())
            .collect();
        assert!(pointers.contains(&"/displayName"), "got {pointers:?}");
        assert!(pointers.contains(&"/email"), "got {pointers:?}");
        assert!(pointers.contains(&"/age"), "got {pointers:?}");
    }

    #[tokio::test]
    async fn repeated_query_parameters_filter_by_several_roles() {
        let client = client();
        for (name, role) in [("Ada", "admin"), ("Grace", "member"), ("Alan", "guest")] {
            client
                .post_json(
                    "/api/v1/users",
                    &format!(
                        r#"{{"displayName":"{name}","email":"{}@example.com","roles":["{role}"]}}"#,
                        name.to_lowercase()
                    ),
                )
                .await
                .assert_status(StatusCode::CREATED);
        }

        let response = client.get("/api/v1/users?role=admin&role=guest").await;
        assert_eq!(response.json()["total"], 2);
    }

    #[tokio::test]
    async fn deleting_requires_an_admin_token() {
        let client = client();
        client
            .post_json(
                "/api/v1/users",
                r#"{"displayName":"Ada","email":"ada@example.com"}"#,
            )
            .await;

        client
            .delete("/api/v1/users/1")
            .await
            .assert_status(StatusCode::UNAUTHORIZED);

        client
            .send(
                TestRequest::new()
                    .method("DELETE")
                    .uri("/api/v1/users/1")
                    .header("authorization", "Bearer wrong"),
            )
            .await
            .assert_status(StatusCode::FORBIDDEN);

        client
            .send(
                TestRequest::new()
                    .method("DELETE")
                    .uri("/api/v1/users/1")
                    .header("authorization", "Bearer admin-token"),
            )
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn the_document_describes_what_the_server_actually_does() {
        let client = client();
        let document = client.openapi();

        // Paths carry the mount prefix.
        assert!(document.paths.contains_key("/api/v1/users/{id}"));

        // The validation constraints reached the schema.
        let create = &document.components.schemas["CreateUser"];
        assert_eq!(create.properties["displayName"].max_length, Some(64));
        assert_eq!(create.properties["email"].format.as_deref(), Some("email"));

        // The generic page type is registered per instantiation.
        assert!(document.components.schemas.contains_key("Page_User"));

        // The auth dependency put its requirement in the document, and the
        // scheme it names was registered automatically.
        let delete = document.paths["/api/v1/users/{id}"]
            .delete
            .as_ref()
            .unwrap();
        assert!(delete.security.as_ref().unwrap()[0].contains_key("bearerAuth"));
        assert!(document
            .components
            .security_schemes
            .contains_key("bearerAuth"));

        // Doc comments became prose.
        let list = document.paths["/api/v1/users"].get.as_ref().unwrap();
        assert_eq!(list.summary.as_deref(), Some("List users."));
        assert_eq!(list.tags, vec!["users"]);
    }

    #[tokio::test]
    async fn the_docs_endpoints_are_served() {
        let client = client();
        client
            .get("/openapi.json")
            .await
            .assert_status(StatusCode::OK);
        let docs = client.get("/docs").await;
        docs.assert_status(StatusCode::OK);
        assert!(docs.text().contains("api-reference"));
    }
}
