//! Server-sent events.
//!
//! The streaming half of "async features": a handler returns a `Stream` and
//! the framework keeps the connection open, formats each item as an SSE frame,
//! and documents the endpoint as `text/event-stream`.

use crate::body::{BoxError, ResBody};
use crate::operation::{OperationContext, OperationOutput};
use crate::response::{IntoResponse, Response};
use bytes::Bytes;
pub use futures_core::Stream;
use futures_util::StreamExt;
use http::{header, HeaderValue};
use std::time::Duration;
use velo_openapi::{MediaType, Schema};

/// One server-sent event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    id: Option<String>,
    name: Option<String>,
    data: String,
    retry: Option<Duration>,
    comment: Option<String>,
}

impl Event {
    /// An event carrying a raw string payload.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            ..Default::default()
        }
    }

    /// An event whose payload is `value` serialised as JSON.
    pub fn json<T: serde::Serialize>(value: &T) -> Result<Self, serde_json::Error> {
        Ok(Self::data(serde_json::to_string(value)?))
    }

    /// A comment-only frame. Useful as a keep-alive that clients ignore.
    pub fn comment(text: impl Into<String>) -> Self {
        Self {
            comment: Some(text.into()),
            ..Default::default()
        }
    }

    /// Sets the event name, which clients subscribe to by name.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the event id, which the client returns in `Last-Event-ID` when it
    /// reconnects.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Tells the client how long to wait before reconnecting.
    pub fn retry(mut self, after: Duration) -> Self {
        self.retry = Some(after);
        self
    }

    /// Encodes the event in the `text/event-stream` wire format.
    pub fn encode(&self) -> Bytes {
        let mut out = String::with_capacity(self.data.len() + 32);

        if let Some(comment) = &self.comment {
            for line in comment.split('\n') {
                out.push(':');
                out.push_str(line);
                out.push('\n');
            }
        }
        if let Some(id) = &self.id {
            // A newline in an id would forge a frame boundary.
            out.push_str("id:");
            out.push_str(&sanitise(id));
            out.push('\n');
        }
        if let Some(name) = &self.name {
            out.push_str("event:");
            out.push_str(&sanitise(name));
            out.push('\n');
        }
        if let Some(retry) = &self.retry {
            out.push_str("retry:");
            out.push_str(&retry.as_millis().to_string());
            out.push('\n');
        }
        if !self.data.is_empty() || self.comment.is_none() {
            // Multi-line payloads are sent as repeated `data:` lines, which
            // the client rejoins with newlines.
            for line in self.data.split('\n') {
                out.push_str("data:");
                out.push_str(line.trim_end_matches('\r'));
                out.push('\n');
            }
        }
        out.push('\n');
        Bytes::from(out)
    }
}

/// Strips characters that would end a field line early.
fn sanitise(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

/// A nameable stream of events.
///
/// A handler's return type has to be nameable for the operation to be
/// documented, and `impl Stream` is not. [`Sse::boxed`] produces this so
/// handlers can write `-> Sse<EventStream>`.
pub type EventStream = std::pin::Pin<Box<dyn Stream<Item = Result<Event, BoxError>> + Send>>;

/// A `text/event-stream` response.
pub struct Sse<S> {
    stream: S,
    keep_alive: Option<Duration>,
}

impl Sse<EventStream> {
    /// Wraps a stream, erasing its type so the handler's signature can name it.
    pub fn boxed<S, E>(stream: S) -> Self
    where
        S: Stream<Item = Result<Event, E>> + Send + 'static,
        E: Into<BoxError> + Send + 'static,
    {
        Sse::new(Box::pin(stream.map(|item| item.map_err(Into::into))) as EventStream)
    }

    /// An infallible stream of events.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Event> + Send + 'static,
    {
        Sse::boxed(stream.map(Ok::<Event, std::convert::Infallible>))
    }
}

impl<S> Sse<S> {
    /// Wraps a stream of events.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            keep_alive: Some(Duration::from_secs(15)),
        }
    }

    /// Sends a comment frame whenever the stream is idle for this long, which
    /// keeps proxies from closing the connection.
    pub fn keep_alive(mut self, interval: Duration) -> Self {
        self.keep_alive = Some(interval);
        self
    }

    /// Turns off keep-alive frames.
    pub fn without_keep_alive(mut self) -> Self {
        self.keep_alive = None;
        self
    }
}

impl<S> std::fmt::Debug for Sse<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sse")
            .field("keep_alive", &self.keep_alive)
            .finish_non_exhaustive()
    }
}

impl<S, E> IntoResponse for Sse<S>
where
    S: Stream<Item = Result<Event, E>> + Send + 'static,
    E: Into<BoxError> + Send + 'static,
{
    fn into_response(self) -> Response {
        let encoded = self
            .stream
            .map(|item| item.map(|event| event.encode()).map_err(Into::into));

        let body = match self.keep_alive {
            None => ResBody::stream(encoded),
            Some(interval) => {
                let comment = Event::comment("keep-alive").encode();
                ResBody::stream(KeepAlive {
                    inner: Box::pin(encoded),
                    timer: Box::pin(tokio::time::sleep(interval)),
                    interval,
                    comment,
                })
            }
        };

        let mut response = Response::new(body);
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        // Buffering an event stream defeats the point of it.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        // nginx buffers proxied responses by default, which would hold every
        // event until the stream ended.
        headers.insert(
            http::HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        );
        response
    }
}

impl<S> OperationOutput for Sse<S> {
    fn describe(ctx: &mut OperationContext<'_>) {
        let mut response = velo_openapi::Response::new(
            "A `text/event-stream` of server-sent events, held open until the \
             client disconnects.",
        );
        response.content.insert(
            "text/event-stream".into(),
            MediaType::new(Schema::of_type("string")),
        );
        ctx.add_response(200, response);
    }
}

/// Emits a comment frame when the inner stream goes quiet.
///
/// Both fields are already boxed and pinned, which makes the adapter itself
/// `Unpin` and the poll logic straightforward.
struct KeepAlive<S> {
    inner: std::pin::Pin<Box<S>>,
    timer: std::pin::Pin<Box<tokio::time::Sleep>>,
    interval: Duration,
    comment: Bytes,
}

impl<S> Stream for KeepAlive<S>
where
    S: Stream<Item = Result<Bytes, BoxError>>,
{
    type Item = Result<Bytes, BoxError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::future::Future;
        use std::task::Poll;

        let this = self.get_mut();

        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(item) => {
                // Any real traffic resets the idle timer.
                this.timer.set(tokio::time::sleep(this.interval));
                Poll::Ready(item)
            }
            Poll::Pending => match this.timer.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    this.timer.set(tokio::time::sleep(this.interval));
                    Poll::Ready(Some(Ok(this.comment.clone())))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn encoded(event: Event) -> String {
        String::from_utf8(event.encode().to_vec()).unwrap()
    }

    #[test]
    fn a_plain_event_is_one_data_line() {
        assert_eq!(encoded(Event::data("hello")), "data:hello\n\n");
    }

    #[test]
    fn multi_line_payloads_become_repeated_data_lines() {
        assert_eq!(encoded(Event::data("one\ntwo")), "data:one\ndata:two\n\n");
    }

    #[test]
    fn name_id_and_retry_precede_the_payload() {
        let event = Event::data("x")
            .named("tick")
            .id("7")
            .retry(Duration::from_secs(2));
        assert_eq!(encoded(event), "id:7\nevent:tick\nretry:2000\ndata:x\n\n");
    }

    #[test]
    fn newlines_in_metadata_cannot_forge_a_frame() {
        let wire = encoded(Event::data("x").named("a\nevent:injected"));
        // The payload stays inside the one `event:` line rather than starting
        // a second field, so a client sees a single oddly-named event.
        let field_lines = wire.lines().filter(|l| l.starts_with("event:")).count();
        assert_eq!(field_lines, 1);
        assert!(wire.starts_with("event:a event:injected\n"));

        // The same holds for ids, where a forged newline would let a client
        // resume from an attacker-chosen position.
        let wire = encoded(Event::data("x").id("1\ndata:forged"));
        assert_eq!(wire.lines().filter(|l| l.starts_with("data:")).count(), 1);
    }

    #[test]
    fn comments_are_ignorable_frames() {
        assert_eq!(encoded(Event::comment("keep-alive")), ":keep-alive\n\n");
    }

    #[test]
    fn json_events_carry_serialised_payloads() {
        #[derive(serde::Serialize)]
        struct Tick {
            n: u32,
        }
        assert_eq!(
            encoded(Event::json(&Tick { n: 3 }).unwrap()),
            "data:{\"n\":3}\n\n"
        );
    }

    #[tokio::test]
    async fn the_response_is_an_unbuffered_event_stream() {
        let stream = futures_util::stream::iter(vec![
            Ok::<_, std::convert::Infallible>(Event::data("a")),
            Ok(Event::data("b")),
        ]);
        let response = Sse::new(stream).without_keep_alive().into_response();

        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"data:a\n\ndata:b\n\n");
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_stream_emits_keep_alive_comments() {
        // A stream that never yields: only the keep-alive timer produces
        // anything, which is exactly the case proxies would otherwise kill.
        let stream = futures_util::stream::pending::<Result<Event, std::convert::Infallible>>();
        let response = Sse::new(stream)
            .keep_alive(Duration::from_secs(1))
            .into_response();

        let mut body = response.into_body();
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("keep-alive should fire")
            .expect("a frame")
            .unwrap();
        assert_eq!(&frame.into_data().unwrap()[..], b":keep-alive\n\n");
    }
}
