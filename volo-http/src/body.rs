use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::{ready, Stream};
use http_body::{Frame, SizeHint};
use http_body_util::{combinators::BoxBody, Full, StreamBody};
use motore::BoxError;
use pin_project::pin_project;

// The `futures_util::stream::BoxStream` does not have `Sync`
type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + Sync + 'a>>;

#[pin_project(project = BodyProj)]
pub enum Body {
    Full(#[pin] Full<Bytes>),
    Stream(#[pin] StreamBody<BoxStream<'static, Result<Frame<Bytes>, BoxError>>>),
    Body(#[pin] BoxBody<Bytes, BoxError>),
}

impl Body {
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Frame<Bytes>, BoxError>> + Send + Sync + 'static,
    {
        Self::Stream(StreamBody::new(Box::pin(stream)))
    }

    pub fn from_body<B>(body: B) -> Self
    where
        B: http_body::Body<Data = Bytes, Error = BoxError> + Send + Sync + 'static,
    {
        Self::Body(BoxBody::new(body))
    }
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.project() {
            BodyProj::Full(full) => {
                // Convert `Infallible` to `BoxError`
                Poll::Ready(ready!(full.poll_frame(cx)).map(|res| Ok(res?)))
            }
            BodyProj::Stream(stream) => stream.poll_frame(cx),
            BodyProj::Body(body) => body.poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Full(full) => full.is_end_stream(),
            Self::Stream(stream) => stream.is_end_stream(),
            Self::Body(body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Full(full) => full.size_hint(),
            Self::Stream(stream) => http_body::Body::size_hint(stream),
            Self::Body(body) => body.size_hint(),
        }
    }
}

impl From<()> for Body {
    fn from(_: ()) -> Self {
        Self::Full(Full::new(Bytes::new()))
    }
}

impl From<&'static str> for Body {
    fn from(value: &'static str) -> Self {
        Self::Full(Full::new(Bytes::from_static(value.as_bytes())))
    }
}

impl From<Vec<u8>> for Body {
    fn from(value: Vec<u8>) -> Self {
        Self::Full(Full::new(Bytes::from(value)))
    }
}

impl From<Bytes> for Body {
    fn from(value: Bytes) -> Self {
        Self::Full(Full::new(value))
    }
}

impl From<FastStr> for Body {
    fn from(value: FastStr) -> Self {
        Self::Full(Full::new(value.into_bytes()))
    }
}

impl From<String> for Body {
    fn from(value: String) -> Self {
        Self::Full(Full::new(Bytes::from(value)))
    }
}
