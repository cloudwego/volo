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
use futures_util::stream::Stream;
use http_body::{Frame, SizeHint};
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::Incoming;
use linkedbytes::{LinkedBytes, Node};
use pin_project::pin_project;
#[cfg(feature = "json")]
use serde::de::DeserializeOwned;

use crate::error::BoxError;

// The `futures_util::stream::BoxStream` does not have `Sync`
type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + Sync + 'a>>;

/// An implementation for [`http_body::Body`].
#[pin_project]
pub struct Body {
    #[pin]
    repr: BodyRepr,
}

#[pin_project(project = BodyProj)]
enum BodyRepr {
    /// Complete [`Bytes`], with a certain size and content
    Full(#[pin] Full<Bytes>),
    /// Wrapper of [`Incoming`], it usually appears in request of server or response of client.
    ///
    /// Althrough [`Incoming`] implements [`http_body::Body`], the type is so commonly used, we
    /// wrap it here as [`BodyRepr::Hyper`] to avoid cost of [`Box`] with dynamic dispatch.
    Hyper(#[pin] Incoming),
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
        Self {
            repr: BodyRepr::Full(Full::new(Bytes::new())),
        }
    }

    /// Create a body by [`Incoming`].
    ///
    /// Compared to [`Body::from_body`], this function avoids overhead of allocating by [`Box`]
    /// and dynamic dispatch by [`dyn http_body::Body`][http_body::Body].
    pub fn from_incoming(incoming: Incoming) -> Self {
        Self {
            repr: BodyRepr::Hyper(incoming),
        }
    }

    /// Create a body by a [`Stream`] with `Item = Result<Frame<Bytes>, BoxError>`.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Frame<Bytes>, BoxError>> + Send + Sync + 'static,
    {
        Self {
            repr: BodyRepr::Stream(StreamBody::new(Box::pin(stream))),
        }
    }

    /// Create a body by another [`http_body::Body`] instance.
    pub fn from_body<B>(body: B) -> Self
    where
        B: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        B::Error: Into<BoxError>,
    {
        Self {
            repr: BodyRepr::Body(BoxBody::new(body.map_err(Into::into))),
        }
    }
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.project().repr.project() {
            BodyProj::Full(full) => http_body::Body::poll_frame(full, cx).map_err(BoxError::from),
            BodyProj::Hyper(incoming) => {
                http_body::Body::poll_frame(incoming, cx).map_err(BoxError::from)
            }
            BodyProj::Stream(stream) => http_body::Body::poll_frame(stream, cx),
            BodyProj::Body(body) => http_body::Body::poll_frame(body, cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.repr {
            BodyRepr::Full(full) => http_body::Body::is_end_stream(full),
            BodyRepr::Hyper(incoming) => http_body::Body::is_end_stream(incoming),
            BodyRepr::Stream(stream) => http_body::Body::is_end_stream(stream),
            BodyRepr::Body(body) => http_body::Body::is_end_stream(body),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.repr {
            BodyRepr::Full(full) => http_body::Body::size_hint(full),
            BodyRepr::Hyper(incoming) => http_body::Body::size_hint(incoming),
            BodyRepr::Stream(stream) => http_body::Body::size_hint(stream),
            BodyRepr::Body(body) => http_body::Body::size_hint(body),
        }
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.repr {
            BodyRepr::Full(_) => f.write_str("Body::Full"),
            BodyRepr::Hyper(_) => f.write_str("Body::Hyper"),
            BodyRepr::Stream(_) => f.write_str("Body::Stream"),
            BodyRepr::Body(_) => f.write_str("Body::Body"),
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

impl Error for BodyConvertError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(feature = "json")]
            Self::JsonDeserializeError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<()> for Body {
    fn from(_: ()) -> Self {
        Self::empty()
    }
}

impl From<&'static str> for Body {
    fn from(value: &'static str) -> Self {
        Self {
            repr: BodyRepr::Full(Full::new(Bytes::from_static(value.as_bytes()))),
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(value: Vec<u8>) -> Self {
        Self {
            repr: BodyRepr::Full(Full::new(Bytes::from(value))),
        }
    }
}

impl From<Bytes> for Body {
    fn from(value: Bytes) -> Self {
        Self {
            repr: BodyRepr::Full(Full::new(value)),
        }
    }
}

impl From<FastStr> for Body {
    fn from(value: FastStr) -> Self {
        Self {
            repr: BodyRepr::Full(Full::new(value.into_bytes())),
        }
    }
}

impl From<String> for Body {
    fn from(value: String) -> Self {
        Self {
            repr: BodyRepr::Full(Full::new(Bytes::from(value))),
        }
    }
}

impl From<LinkedBytes> for Body {
    fn from(value: LinkedBytes) -> Self {
        let stream = async_stream::stream! {
            for node in value.into_iter_list() {
                match node {
                    Node::Bytes(bytes) => {
                        yield Ok(Frame::data(bytes));
                    }
                    Node::BytesMut(bytes) => {
                        yield Ok(Frame::data(bytes.freeze()));
                    }
                    Node::FastStr(faststr) => {
                        yield Ok(Frame::data(faststr.into_bytes()));
                    }
                }
            }
        };
        Self {
            repr: BodyRepr::Stream(StreamBody::new(Box::pin(stream))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_from_linked_bytes() {
        let mut bytes = LinkedBytes::new();
        bytes.insert(Bytes::from_static(b"Hello,"));
        bytes.insert_faststr(FastStr::new(" world!"));
        let body = Body::from(bytes);
        assert_eq!(
            body.into_bytes().await.unwrap(),
            Bytes::from_static(b"Hello, world!")
        );
    }
}
