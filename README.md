# velo

**An async Rust web framework with first-class OpenAPI 3.1.**

FastAPI's core insight is that a handler's signature already contains
everything needed to describe the endpoint — so writing that description a
second time is a bug waiting to happen. `velo` takes the same position and
adds what a compiled language makes possible: constraints checked when you
build, not when the first bad request arrives.

```rust
use velo::prelude::*;

#[derive(Schema, serde::Deserialize)]
struct NewUser {
    /// Shown to other people.
    #[validate(min_length = 1, max_length = 64, non_blank)]
    display_name: String,
    #[validate(format = "email")]
    email: String,
}

#[derive(Schema, serde::Serialize)]
struct User { id: u64, display_name: String }

/// Create a user.
#[post("/users", tags = ["users"])]
async fn create_user(
    Json(body): Json<NewUser>,
    State(db): State<Db>,
) -> Result<Created<Json<User>>, ApiError> {
    let user = db.insert(body).await?;
    Ok(Created::at(format!("/users/{}", user.id), Json(user)))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    App::new()
        .title("Users API")
        .version("1.0.0")
        .with_state(Db::connect().await?)
        .mount(routes![create_user])
        .serve(([127, 0, 0, 1], 8080))
        .await
}
```

That program serves the endpoint, `/openapi.json`, and a browsable UI at
`/docs`. There is no spec file, and no annotation restating a type.

---

## Why not just FastAPI

| | FastAPI | velo |
|---|---|---|
| Validation errors | discovered at first request | `#[validate(min_length)]` on a number is a **compile error** |
| Error envelope | ad-hoc `{"detail": ...}` | **RFC 9457** problem details, described in the document |
| Repeated query params | works | works — and `?tag=a&tag=b` deserialises into `Vec<T>`, which `serde_urlencoded` cannot do |
| Dependency visibility | an auth dependency is invisible in the docs unless you remember to declare it | a `Dependency` declares its own security requirement and error responses |
| Dependency caching | per request | per request, keyed by `TypeId` |
| Body size limits | app-server concern | refused from `Content-Length` before a byte is buffered |
| Runtime | interpreter, per-field reflection | monomorphised handlers, one deserialise |

The parts of FastAPI worth keeping are kept: extractors read like function
arguments, doc comments become prose, and the document is always in step with
the server because there is only one place the information lives.

## Layout

| Crate | Contents |
|---|---|
| [`velo`](velo) | the framework: server, router, extractors, responses, middleware, docs endpoints |
| [`velo-openapi`](velo-openapi) | the OpenAPI 3.1 document model and JSON Schema 2020-12 generation. Independent of HTTP |
| [`velo-macros`](velo-macros) | `#[derive(Schema)]`, the method attributes, `routes!` |
| [`examples/basic`](examples/basic) | a complete service — CRUD, auth, SSE, pagination |

## Getting started

```bash
cargo run -p velo-example-basic
```

Then open <http://127.0.0.1:8080/docs>. To see the document without starting a
server:

```bash
cargo run -p velo-example-basic -- --print-openapi
```

## How documentation is generated

Two traits do the work. Every extractor implements `OperationInput`, and every
response type implements `OperationOutput`. The method attribute walks the
handler's argument types and return type and calls them:

```text
async fn get_user(Path(id): Path<u64>, State(db): State<Db>)
    -> Result<Json<User>, ApiError>
                 │           │          │        │      │
                 │           │          │        │      └─ default: problem+json
                 │           │          │        └───────── 200: #/…/schemas/User
                 │           │          └────────────────── combines both arms
                 │           └───────────────────────────── invisible (no-op)
                 └───────────────────────────────────────── parameter `id`,
                                                            integer/int64,
                                                            required
```

Because `Path<u64>` reads the parameter's *name* from the route template, a
scalar path parameter needs no annotation at all. Renaming a field, changing a
type, or deleting an argument changes the document in the same edit.

Implementing a custom response type means implementing both traits — you
cannot add a response shape the docs don't know about.

## Validation

A `#[validate(...)]` rule emits a JSON Schema keyword *and* a runtime check
from one declaration, so the documented constraint and the enforced constraint
cannot disagree.

```rust
#[derive(Schema, serde::Deserialize)]
struct Signup {
    #[validate(min_length = 3, max_length = 20, pattern = "^[a-z0-9_]+$")]
    username: String,
    #[validate(format = "email")]
    email: String,
    #[validate(range(min = 13, max = 130))]
    age: Option<u8>,      // checked only when present
    #[validate(nested)]
    address: Address,     // errors come back as /address/street
}
```

A failure produces a `422` listing every offending field with a JSON Pointer:

```json
{
  "type": "about:blank",
  "title": "Validation failed",
  "status": 422,
  "detail": "2 fields failed validation.",
  "errors": [
    { "pointer": "/username", "code": "min_length",
      "message": "must be at least 3 characters, got 1" },
    { "pointer": "/address/street", "code": "non_blank",
      "message": "must not be blank" }
  ]
}
```

Rules are type-checked. `#[validate(min_length = 3)]` on a `u32` does not
compile, because `u32` has no length — the mistake is caught at build time
rather than becoming a rule that silently never fires.

Available rules: `min_length`, `max_length`, `min_items`, `max_items`,
`minimum`, `maximum`, `exclusive_minimum`, `exclusive_maximum`, `multiple_of`,
`range(min, max)`, `pattern`, `format`, `non_blank`, `nested`, `custom`.

`pattern` requires the `regex` feature, which is on by default; without it a
pattern would be documented but never enforced.

## Dependencies

`Depends<T>` is FastAPI's dependency injection. A dependency can use other
extractors, is resolved at most once per request, and describes itself:

```rust
#[derive(Clone)]
struct CurrentUser(String);

impl Dependency for CurrentUser {
    async fn resolve(req: &mut Request) -> Result<Self, ApiError> {
        let Bearer(token) = Bearer::from_request(req).await?;
        let State(db) = State::<Db>::from_request(req).await?;
        db.user_for_token(&token).await
            .ok_or_else(|| ApiError::unauthorized("Unknown token."))
    }

    fn describe(ctx: &mut OperationContext<'_>) {
        <Bearer as OperationInput>::describe(ctx);   // inherit the 401 + scheme
    }
}
```

Three handlers' worth of sub-dependencies asking for `CurrentUser` produce one
database round-trip, and the operation shows `bearerAuth` in its security
requirements. The `bearerAuth` scheme is registered in `components`
automatically.

## Streaming

Handlers can hold a connection open. `Sse<EventStream>` formats each item as a
server-sent event, sends keep-alive comments when idle, and documents the
endpoint as `text/event-stream`:

```rust
#[get("/countdown/{from}")]
async fn countdown(Path(from): Path<u8>) -> Sse<EventStream> {
    let stream = futures_util::stream::unfold(from, |n| async move {
        if n == 0 { return None }
        tokio::time::sleep(Duration::from_secs(1)).await;
        Some((Event::data(n.to_string()).named("tick"), n - 1))
    });
    Sse::from_stream(stream)
}
```

The return type is `Sse<EventStream>` rather than `Sse<impl Stream>` because
the generated operation must name the type; `Sse::boxed` erases any stream
into it. Trying to return `impl Trait` from a handler is a compile error that
says so.

## Testing

`TestClient` dispatches through the same code path the server uses — real
routing, real extractors, real middleware:

```rust
#[tokio::test]
async fn validation_failures_name_every_bad_field() {
    let client = TestClient::new(app());
    let response = client
        .post_json("/users", r#"{"displayName":"   ","email":"nope"}"#)
        .await;

    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.json()["errors"][0]["pointer"], "/displayName");
}
```

The document itself is assertable, which is how you keep a published contract
from moving under you:

```rust
let document = client.openapi();
assert_eq!(
    document.components.schemas["CreateUser"].properties["email"].format.as_deref(),
    Some("email")
);
```

## Middleware

An async layer wrapped around a handler. `Next::run` continues inward; not
calling it short-circuits.

```rust
App::new()
    .layer(Arc::new(RequestId::new()))
    .layer(Arc::new(Logger::new()))
    .layer(Arc::new(Cors::permissive().allow_origins(["https://app.example"])))
    .layer(Arc::new(Timeout::seconds(30)))
```

Built in: `RequestId`, `Logger`, `Cors`, `Timeout`, `CatchPanic` (mounted by
default, so a panicking handler is a 500 rather than a dropped connection).
`middleware::from_fn` builds one from a closure.

Routers nest, and prefixes, tags, and layers compose:

```rust
App::new()
    .nest("/api/v1", Router::new()
        .mount(routes![list_users, create_user])
        .tag("users")
        .layer(Arc::new(RequireAuth)))
```

## Routing

Templates use `{name}` for one segment and `{*name}` for the rest of the path.
Static segments beat parameters, and parameters beat catch-alls, **at every
level with backtracking** — so `/files/latest` stays reachable next to
`/files/{*path}`.

Conflicts are refused at build time rather than resolved by registration
order: a duplicated `(method, path)`, two different parameter names at the same
position, or a catch-all that is not last.

Other behaviour worth knowing:

- A known path with an unknown method is a `405` with an accurate `Allow`
  header, not a `404`.
- `HEAD` falls back to the `GET` handler with the body dropped.
- `OPTIONS` on a known path answers `204` with `Allow`.
- Trailing slashes do not change the match.
- `%2F` in a segment cannot smuggle an extra path segment past the router:
  matching happens on the raw path and decoding after.

## Documentation endpoints

By default: [Scalar](https://scalar.com) at `/docs`, Redoc at `/redoc`, and the
document at `/openapi.json`. Swagger UI is available too.

```rust
App::new()
    .docs(Docs::only(Renderer::SwaggerUi)
        .self_hosted_assets("/static/docs"))   // for air-gapped networks
```

`Docs::json_only()` serves the document with no UI; `without_docs()` serves
neither, while `App::openapi()` still returns the document programmatically —
useful for generating clients in CI.

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `macros` | ✓ | `#[derive(Schema)]`, method attributes, `routes!` |
| `regex` | ✓ | enforces `#[validate(pattern)]` at runtime |
| `tracing` | | emits `tracing` events instead of stderr logging |
| `uuid` | | `JsonSchema` for `uuid::Uuid` |
| `chrono` | | `JsonSchema` for `chrono` date and time types |
| `full` | | all of the above |

## Status

Early. The API surface is complete enough to build real services against, and
the behaviour described here is covered by tests:

```bash
cargo test --workspace --all-features
```

188 tests: 125 unit, 28 on derive output, 14 over a real TCP socket, 14 in
`velo-openapi`, plus the example service's own suite.

Known limits, stated plainly:

- **Request bodies are buffered**, subject to `body_limit` (2 MiB by default).
  There is no streaming-request extractor yet, so large uploads are out of
  scope for now.
- **No `multipart/form-data`.** File uploads need it; it is the largest gap.
- **No WebSocket support.** SSE covers server-to-client streaming only.
- **`Path<T>` deserialises from strings**, so a path parameter cannot be a
  nested structure. That matches what a URL can express.
- Route templates are `&'static str`; nesting leaks a joined string per route
  at startup. Bounded by route count, never repeated.
- The crate name is a placeholder — check availability before publishing.

## License

MIT OR Apache-2.0.
