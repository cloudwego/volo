//! HTTP Body implementation for [`http_body::Body`]
//!
//! See [`Body`] for more details.

use std::{
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::{ready, Stream};
use http_body::{Frame, SizeHint};
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
pub use hyper::body::Incoming;
use motore::BoxError;
use pin_project::pin_project;
#[cfg(feature = "json")]
use serde::de::DeserializeOwned;

// The `futures_util::stream::BoxStream` does not have `Sync`
type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + Sync + 'a>>;

/// An implementation for [`http_body::Body`].
#[pin_project(project = BodyProj)]
pub enum Body {
    /// Complete [`Bytes`], with a certain size and content
    Full(#[pin] Full<Bytes>),
    /// Boxed stream with `Item = Result<Frame<Bytes>, BoxError>`
    Stream(#[pin] StreamBody<BoxStream<'static, Result<Frame<Bytes>, BoxError>>>),
    /// Boxed [`http_body::Body`]
    Body(#[pin] BoxBody<Bytes, BoxError>),
}

impl Default for Body {
    fn default() -> Self {
        Body::empty()
    }
}

impl Body {
    /// Create an empty body.
    pub fn empty() -> Self {
        Self::Full(Full::new(Bytes::new()))
    }

    /// Create a body by a [`Stream`] with `Item = Result<Frame<Bytes>, BoxError>`.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Frame<Bytes>, BoxError>> + Send + Sync + 'static,
    {
        Self::Stream(StreamBody::new(Box::pin(stream)))
    }

    /// Create a body by another [`http_body::Body`] instance.
    pub fn from_body<B>(body: B) -> Self
    where
        B: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        B::Error: Into<BoxError>,
    {
        Self::Body(BoxBody::new(body.map_err(Into::into)))
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

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(_) => f.write_str("Body::Full"),
            Self::Stream(_) => f.write_str("Body::Stream"),
            Self::Body(_) => f.write_str("Body::Body"),
        }
    }
}

mod sealed {
    pub trait SealedBody
    where
        Self: http_body::Body + Sized + Send,
        Self::Data: Send,
    {
    }

    impl<T> SealedBody for T
    where
        T: http_body::Body + Send,
        T::Data: Send,
    {
    }
}

/// An extend trait for [`http_body::Body`] that can converting a body to other types
pub trait BodyConversion: sealed::SealedBody
where
    <Self as http_body::Body>::Data: Send,
{
    /// Consume a body and convert it into [`Bytes`].
    fn into_bytes(self) -> impl Future<Output = Result<Bytes, BodyConvertError>> + Send {
        async {
            Ok(self
                .collect()
                .await
                .map_err(|_| BodyConvertError::BodyCollectionError)?
                .to_bytes())
        }
    }

    /// Consume a body and convert it into [`Vec<u8>`].
    fn into_vec(self) -> impl Future<Output = Result<Vec<u8>, BodyConvertError>> + Send {
        async { Ok(self.into_bytes().await?.into()) }
    }

    /// Consume a body and convert it into [`String`].
    fn into_string(self) -> impl Future<Output = Result<String, BodyConvertError>> + Send {
        async {
            let vec = self.into_vec().await?;

            // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
            let _ =
                simdutf8::basic::from_utf8(&vec).map_err(|_| BodyConvertError::StringUtf8Error)?;
            Ok(unsafe { String::from_utf8_unchecked(vec) })
        }
    }

    /// Consume a body and convert it into [`String`].
    ///
    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    unsafe fn into_string_unchecked(
        self,
    ) -> impl Future<Output = Result<String, BodyConvertError>> + Send {
        async {
            let vec = self.into_vec().await?;

            Ok(String::from_utf8_unchecked(vec))
        }
    }

    /// Consume a body and convert it into [`FastStr`].
    fn into_faststr(self) -> impl Future<Output = Result<FastStr, BodyConvertError>> + Send {
        async {
            let bytes = self.into_bytes().await?;

            // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
            let _ = simdutf8::basic::from_utf8(&bytes)
                .map_err(|_| BodyConvertError::StringUtf8Error)?;
            Ok(unsafe { FastStr::from_bytes_unchecked(bytes) })
        }
    }

    /// Consume a body and convert it into [`FastStr`].
    ///
    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    unsafe fn into_faststr_unchecked(
        self,
    ) -> impl Future<Output = Result<FastStr, BodyConvertError>> + Send {
        async {
            let bytes = self.into_bytes().await?;

            Ok(FastStr::from_bytes_unchecked(bytes))
        }
    }

    /// Consume a body and convert it into an instance with [`DeserializeOwned`].
    #[cfg(feature = "json")]
    fn into_json<T>(self) -> impl Future<Output = Result<T, BodyConvertError>> + Send
    where
        T: DeserializeOwned,
    {
        async {
            let bytes = self.into_bytes().await?;
            crate::utils::json::deserialize(&bytes).map_err(BodyConvertError::JsonDeserializeError)
        }
    }
}

impl<T> BodyConversion for T
where
    T: sealed::SealedBody,
    <T as http_body::Body>::Data: Send,
{
}

/// General error for polling [`http_body::Body`] or converting the [`Bytes`] just polled.
#[derive(Debug)]
pub enum BodyConvertError {
    /// Failed to collect the body
    BodyCollectionError,
    /// The body is not a valid utf-8 string
    StringUtf8Error,
    /// Failed to deserialize the json
    #[cfg(feature = "json")]
    JsonDeserializeError(crate::utils::json::Error),
}

impl fmt::Display for BodyConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyCollectionError => f.write_str("failed to collect body"),
            Self::StringUtf8Error => f.write_str("body is not a valid string"),
            #[cfg(feature = "json")]
            Self::JsonDeserializeError(e) => write!(f, "failed to deserialize body: {e}"),
        }
    }
}

impl Error for BodyConvertError {}

impl From<()> for Body {
    fn from(_: ()) -> Self {
        Self::empty()
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
