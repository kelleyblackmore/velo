//! The response body type.

use bytes::Bytes;
use futures_core::Stream;
use http_body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Boxed error type used throughout the crate for opaque failures.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A stream of body chunks.
pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, BoxError>> + Send>>;

/// The body of an outgoing response.
///
/// Buffered responses avoid any allocation beyond the payload itself;
/// streaming responses (SSE, chunked downloads) carry a boxed stream.
#[derive(Default)]
pub enum ResBody {
    /// No payload at all — `204`, `304`, and responses to `HEAD`.
    #[default]
    Empty,
    /// A fully buffered payload of known length.
    Full(Bytes),
    /// A stream of chunks of unknown total length.
    Stream(BodyStream),
}

impl ResBody {
    /// Wraps an already-buffered payload.
    pub fn full(bytes: impl Into<Bytes>) -> Self {
        let bytes = bytes.into();
        if bytes.is_empty() {
            ResBody::Empty
        } else {
            ResBody::Full(bytes)
        }
    }

    /// Wraps a stream of chunks.
    pub fn stream<S, E>(stream: S) -> Self
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
        E: Into<BoxError> + 'static,
    {
        use futures_util::StreamExt;
        ResBody::Stream(Box::pin(stream.map(|r| r.map_err(Into::into))))
    }

    /// The exact payload length, when it is known up front.
    pub fn len(&self) -> Option<usize> {
        match self {
            ResBody::Empty => Some(0),
            ResBody::Full(b) => Some(b.len()),
            ResBody::Stream(_) => None,
        }
    }

    /// True when this body is statically known to carry no bytes.
    pub fn is_empty(&self) -> bool {
        matches!(self, ResBody::Empty)
    }
}

impl std::fmt::Debug for ResBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResBody::Empty => f.write_str("ResBody::Empty"),
            ResBody::Full(b) => f.debug_tuple("ResBody::Full").field(&b.len()).finish(),
            ResBody::Stream(_) => f.write_str("ResBody::Stream(..)"),
        }
    }
}

impl From<Bytes> for ResBody {
    fn from(b: Bytes) -> Self {
        ResBody::full(b)
    }
}

impl From<Vec<u8>> for ResBody {
    fn from(b: Vec<u8>) -> Self {
        ResBody::full(b)
    }
}

impl From<String> for ResBody {
    fn from(s: String) -> Self {
        ResBody::full(s)
    }
}

impl From<&'static str> for ResBody {
    fn from(s: &'static str) -> Self {
        ResBody::full(Bytes::from_static(s.as_bytes()))
    }
}

impl Body for ResBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            ResBody::Empty => Poll::Ready(None),
            ResBody::Full(bytes) => {
                if bytes.is_empty() {
                    Poll::Ready(None)
                } else {
                    // Hand the buffer over wholesale and leave `Empty` behind so
                    // the next poll terminates.
                    let chunk = std::mem::take(bytes);
                    Poll::Ready(Some(Ok(Frame::data(chunk))))
                }
            }
            ResBody::Stream(stream) => stream
                .as_mut()
                .poll_next(cx)
                .map(|opt| opt.map(|res| res.map(Frame::data))),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            ResBody::Empty => true,
            ResBody::Full(b) => b.is_empty(),
            ResBody::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            ResBody::Empty => SizeHint::with_exact(0),
            ResBody::Full(b) => SizeHint::with_exact(b.len() as u64),
            ResBody::Stream(_) => SizeHint::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn full_body_yields_exactly_one_frame() {
        let body = ResBody::full("hello");
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"hello");
    }

    #[test]
    fn empty_payloads_collapse_to_empty_variant() {
        assert!(ResBody::full("").is_empty());
        assert_eq!(ResBody::full("").len(), Some(0));
    }

    #[tokio::test]
    async fn streaming_bodies_have_no_known_length() {
        let s = futures_util::stream::iter(vec![
            Ok::<_, BoxError>(Bytes::from("a")),
            Ok(Bytes::from("b")),
        ]);
        let body = ResBody::stream(s);
        assert_eq!(body.len(), None);
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"ab");
    }
}
