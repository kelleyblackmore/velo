//! End-to-end tests over a real TCP socket.
//!
//! The in-process `TestClient` covers routing and extraction; these cover the
//! things only a socket can show — chunked streaming, `HEAD` semantics,
//! `Allow` headers, body limits, and graceful shutdown.
//!
//! Requests are written by hand rather than through an HTTP client so the
//! tests assert on actual bytes on the wire.

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use velo::prelude::*;
use velo::sse::EventStream;

// ---------------------------------------------------------------------------
// a small service
// ---------------------------------------------------------------------------

#[derive(Debug, Schema, serde::Serialize, serde::Deserialize)]
struct Echo {
    message: String,
}

#[get("/hello")]
async fn hello() -> Json<Echo> {
    Json(Echo {
        message: "hi".into(),
    })
}

#[post("/echo")]
async fn echo(Json(body): Json<Echo>) -> Json<Echo> {
    Json(body)
}

#[get("/boom")]
async fn boom() -> Json<Echo> {
    panic!("this handler is broken")
}

#[get("/stream")]
async fn stream() -> Sse<EventStream> {
    let events = futures_util::stream::iter(vec![
        Event::data("one").named("tick"),
        Event::data("two").named("tick"),
    ]);
    Sse::from_stream(events).without_keep_alive()
}

#[get("/files/{*path}")]
async fn files(Path(path): Path<String>) -> String {
    path
}

#[derive(Debug, Schema, serde::Serialize)]
struct Upload {
    caption: String,
    file_name: String,
    bytes: usize,
    /// Proves the payload survived the wire byte for byte.
    checksum: u32,
}

#[post("/upload")]
async fn upload(form: Multipart) -> Result<Json<Upload>, ApiError> {
    let file = form.require_file("file")?;
    Ok(Json(Upload {
        caption: form.require_text("caption")?.to_owned(),
        file_name: file.file_name().unwrap_or_default().to_owned(),
        bytes: file.len(),
        checksum: file.bytes().iter().map(|b| *b as u32).sum(),
    }))
}

fn app() -> App {
    App::new()
        .title("Wire tests")
        .version("0.1.0")
        .body_limit(1024)
        .mount(routes![hello, echo, boom, stream, files, upload])
}

// ---------------------------------------------------------------------------
// a hand-rolled client
// ---------------------------------------------------------------------------

struct Server {
    addr: std::net::SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Server {
    async fn start(app: App) -> Self {
        let bound = app
            .build()
            .expect("routes should not conflict")
            .bind(([127, 0, 0, 1], 0).into())
            .await
            .expect("binding to an ephemeral port should succeed");

        let addr = bound.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(bound.serve_with_shutdown(async {
            let _ = rx.await;
        }));

        Server {
            addr,
            shutdown: Some(tx),
            task: Some(task),
        }
    }

    /// Sends a raw request and returns the complete raw response.
    async fn raw(&self, request: &str) -> String {
        let mut stream = TcpStream::connect(self.addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        stream.flush().await.expect("flush");

        let mut response = Vec::new();
        // The server closes the connection on `Connection: close`, so reading
        // to EOF gives the whole response without parsing lengths.
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
            .await
            .expect("response should arrive")
            .expect("read");

        String::from_utf8_lossy(&response).into_owned()
    }

    async fn request(&self, method: &str, path: &str, extra: &str, body: &str) -> String {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Length: {}\r\n{extra}\r\n{body}",
            body.len()
        );
        self.raw(&request).await
    }

    async fn get(&self, path: &str) -> String {
        self.request("GET", path, "", "").await
    }

    /// Sends a request whose body is arbitrary bytes rather than text.
    async fn request_bytes(
        &self,
        method: &str,
        path: &str,
        content_type: &str,
        body: &[u8],
    ) -> String {
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut request = head.into_bytes();
        request.extend_from_slice(body);

        let mut stream = TcpStream::connect(self.addr).await.expect("connect");
        stream.write_all(&request).await.expect("write");
        stream.flush().await.expect("flush");

        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
            .await
            .expect("response should arrive")
            .expect("read");
        String::from_utf8_lossy(&response).into_owned()
    }

    async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
    }
}

fn status_line(response: &str) -> &str {
    response.lines().next().unwrap_or_default()
}

fn body_of(response: &str) -> &str {
    response.split("\r\n\r\n").nth(1).unwrap_or_default()
}

fn has_header(response: &str, needle: &str) -> bool {
    response
        .split("\r\n\r\n")
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .contains(&needle.to_lowercase())
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_get_returns_json_over_the_wire() {
    let server = Server::start(app()).await;
    let response = server.get("/hello").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    assert!(has_header(&response, "content-type: application/json"));
    assert!(body_of(&response).contains(r#""message":"hi""#));

    server.stop().await;
}

#[tokio::test]
async fn a_post_round_trips_a_body() {
    let server = Server::start(app()).await;
    let response = server
        .request(
            "POST",
            "/echo",
            "Content-Type: application/json\r\n",
            r#"{"message":"pong"}"#,
        )
        .await;

    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    assert!(body_of(&response).contains("pong"));

    server.stop().await;
}

#[tokio::test]
async fn an_unknown_path_is_a_problem_json_404() {
    let server = Server::start(app()).await;
    let response = server.get("/nope").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 404"));
    assert!(has_header(
        &response,
        "content-type: application/problem+json"
    ));
    let body: serde_json::Value = serde_json::from_str(body_of(&response)).unwrap();
    assert_eq!(body["status"], 404);
    assert_eq!(body["title"], "Not Found");

    server.stop().await;
}

#[tokio::test]
async fn a_wrong_method_is_a_405_with_an_allow_header() {
    let server = Server::start(app()).await;
    let response = server.request("DELETE", "/hello", "", "").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 405"));
    assert!(has_header(&response, "allow: GET"));

    server.stop().await;
}

#[tokio::test]
async fn head_reuses_the_get_handler_and_drops_the_body() {
    let server = Server::start(app()).await;
    let response = server.request("HEAD", "/hello", "", "").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    assert!(has_header(&response, "content-type: application/json"));
    assert!(
        body_of(&response).is_empty(),
        "HEAD must not carry a body, got {:?}",
        body_of(&response)
    );

    server.stop().await;
}

#[tokio::test]
async fn options_on_a_known_path_reports_what_is_allowed() {
    let server = Server::start(app()).await;
    let response = server.request("OPTIONS", "/hello", "", "").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 204"));
    assert!(has_header(&response, "allow: GET"));

    server.stop().await;
}

#[tokio::test]
async fn a_panicking_handler_returns_500_and_the_server_survives() {
    let server = Server::start(app()).await;

    let failed = server.get("/boom").await;
    assert!(status_line(&failed).starts_with("HTTP/1.1 500"));

    // The important half: the next request still works.
    let ok = server.get("/hello").await;
    assert!(status_line(&ok).starts_with("HTTP/1.1 200"));

    server.stop().await;
}

#[tokio::test]
async fn an_oversized_body_is_refused_with_413() {
    let server = Server::start(app()).await;
    let huge = "x".repeat(4096);
    let response = server
        .request(
            "POST",
            "/echo",
            "Content-Type: application/json\r\n",
            &format!(r#"{{"message":"{huge}"}}"#),
        )
        .await;

    assert!(
        status_line(&response).starts_with("HTTP/1.1 413"),
        "got {}",
        status_line(&response)
    );

    server.stop().await;
}

#[tokio::test]
async fn events_are_streamed_as_chunks_not_one_buffered_blob() {
    let server = Server::start(app()).await;
    let response = server.get("/stream").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    assert!(has_header(&response, "content-type: text/event-stream"));
    // An unknown-length body must be chunked rather than given a
    // Content-Length, which is what makes it stream.
    assert!(has_header(&response, "transfer-encoding: chunked"));
    assert!(response.contains("event:tick"));
    assert!(response.contains("data:one"));
    assert!(response.contains("data:two"));

    server.stop().await;
}

#[tokio::test]
async fn a_catch_all_captures_the_remaining_path() {
    let server = Server::start(app()).await;
    let response = server.get("/files/a/b/c.txt").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    assert_eq!(body_of(&response), "a/b/c.txt");

    server.stop().await;
}

#[tokio::test]
async fn a_bad_body_is_a_422_naming_the_field() {
    let server = Server::start(app()).await;
    let response = server
        .request(
            "POST",
            "/echo",
            "Content-Type: application/json\r\n",
            r#"{"message":123}"#,
        )
        .await;

    assert!(status_line(&response).starts_with("HTTP/1.1 422"));
    let body: serde_json::Value = serde_json::from_str(body_of(&response)).unwrap();
    assert_eq!(body["errors"][0]["pointer"], "/message");

    server.stop().await;
}

/// Assembles a multipart body. Written by hand so the bytes on the wire are
/// exactly what a browser would send, rather than whatever a helper decides.
fn multipart_body(boundary: &str, caption: &str, file: Option<(&str, &[u8])>) -> Vec<u8> {
    let mut body = Vec::new();

    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"caption\"\r\n\r\n{caption}\r\n"
        )
        .as_bytes(),
    );

    if let Some((name, bytes)) = file {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
                 filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

#[tokio::test]
async fn a_multipart_upload_arrives_intact_over_the_wire() {
    let server = Server::start(app()).await;

    // Every byte value, so anything that mishandles CR, LF, or the `-` that
    // begins a boundary shows up as a checksum mismatch rather than passing.
    let payload: Vec<u8> = (0u8..=255).collect();
    let expected: u32 = payload.iter().map(|b| *b as u32).sum();

    let body = multipart_body("BOUNDARY", "hello", Some(("all-bytes.bin", &payload)));
    let response = server
        .request_bytes(
            "POST",
            "/upload",
            "multipart/form-data; boundary=BOUNDARY",
            &body,
        )
        .await;

    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "{response}"
    );
    let parsed: serde_json::Value = serde_json::from_str(body_of(&response)).unwrap();
    assert_eq!(parsed["caption"], "hello");
    assert_eq!(parsed["file_name"], "all-bytes.bin");
    assert_eq!(parsed["bytes"], 256);
    assert_eq!(parsed["checksum"], expected);

    server.stop().await;
}

#[tokio::test]
async fn a_multipart_body_missing_a_required_part_is_a_422() {
    let server = Server::start(app()).await;
    let body = multipart_body("B", "only text", None);

    let response = server
        .request_bytes("POST", "/upload", "multipart/form-data; boundary=B", &body)
        .await;

    assert!(
        status_line(&response).starts_with("HTTP/1.1 422"),
        "{response}"
    );
    let parsed: serde_json::Value = serde_json::from_str(body_of(&response)).unwrap();
    assert_eq!(parsed["errors"][0]["pointer"], "/file");
    assert_eq!(parsed["errors"][0]["code"], "missing");

    server.stop().await;
}

#[tokio::test]
async fn a_json_body_sent_to_a_multipart_handler_is_a_415() {
    let server = Server::start(app()).await;
    let response = server
        .request(
            "POST",
            "/upload",
            "Content-Type: application/json\r\n",
            "{}",
        )
        .await;

    assert!(
        status_line(&response).starts_with("HTTP/1.1 415"),
        "{response}"
    );

    server.stop().await;
}

#[tokio::test]
async fn the_openapi_document_is_served_and_is_valid_json() {
    let server = Server::start(app()).await;
    let response = server.get("/openapi.json").await;

    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    let document: serde_json::Value = serde_json::from_str(body_of(&response)).unwrap();
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(document["info"]["title"], "Wire tests");
    assert!(document["paths"]["/hello"]["get"].is_object());
    // The catch-all is documented without the routing star.
    assert!(document["paths"]["/files/{path}"].is_object());
    // The upload advertises its media type rather than looking like JSON.
    assert!(
        document["paths"]["/upload"]["post"]["requestBody"]["content"]["multipart/form-data"]
            .is_object()
    );

    server.stop().await;
}

#[tokio::test]
async fn startup_and_shutdown_hooks_run_in_order() {
    let log = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));

    let startup_log = Arc::clone(&log);
    let shutdown_log = Arc::clone(&log);

    let app = app()
        .on_startup(move |_state| async move {
            startup_log.lock().unwrap().push("startup");
            Ok(())
        })
        .on_shutdown(move |_state| async move {
            shutdown_log.lock().unwrap().push("shutdown");
        });

    let server = Server::start(app).await;
    assert_eq!(*log.lock().unwrap(), vec!["startup"]);

    server.stop().await;
    assert_eq!(*log.lock().unwrap(), vec!["startup", "shutdown"]);
}

#[tokio::test]
async fn a_failing_startup_hook_aborts_before_binding() {
    let result = app()
        .on_startup(|_state| async { Err("database is unreachable".to_owned()) })
        .build()
        .expect("routes should not conflict")
        .bind(([127, 0, 0, 1], 0).into())
        .await;

    let error = result.expect_err("startup failure should abort");
    assert!(error.to_string().contains("database is unreachable"));
}
