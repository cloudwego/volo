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
#[cfg(feature = "__json")]
use serde::de::DeserializeOwned;

// The `futures_util::stream::BoxStream` does not have `Sync`
type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + Sync + 'a>>;

#[pin_project(project = BodyProj)]
pub enum Body {
    Full(#[pin] Full<Bytes>),
    Stream(#[pin] StreamBody<BoxStream<'static, Result<Frame<Bytes>, BoxError>>>),
    Body(#[pin] BoxBody<Bytes, BoxError>),
}

impl Default for Body {
    fn default() -> Self {
        Body::empty()
    }
}

impl Body {
    pub fn empty() -> Self {
        Self::Full(Full::new(Bytes::new()))
    }

    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Frame<Bytes>, BoxError>> + Send + Sync + 'static,
    {
        Self::Stream(StreamBody::new(Box::pin(stream)))
    }

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

pub trait BodyConversion
where
    Self: http_body::Body + Sized + Send,
    Self::Data: Send,
{
    fn into_bytes(self) -> impl Future<Output = Result<Bytes, ResponseConvertError>> + Send {
        async {
            Ok(self
                .collect()
                .await
                .map_err(|_| ResponseConvertError::BodyCollectionError)?
                .to_bytes())
        }
    }

    fn into_vec(self) -> impl Future<Output = Result<Vec<u8>, ResponseConvertError>> + Send {
        async { Ok(self.into_bytes().await?.into()) }
    }

    fn into_string(self) -> impl Future<Output = Result<String, ResponseConvertError>> + Send {
        async {
            let vec = self.into_vec().await?;

            // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
            let _ = simdutf8::basic::from_utf8(&vec)
                .map_err(|_| ResponseConvertError::StringUtf8Error)?;
            Ok(unsafe { String::from_utf8_unchecked(vec) })
        }
    }

    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    unsafe fn into_string_unchecked(
        self,
    ) -> impl Future<Output = Result<String, ResponseConvertError>> + Send {
        async {
            let vec = self.into_vec().await?;

            Ok(String::from_utf8_unchecked(vec))
        }
    }

    fn into_faststr(self) -> impl Future<Output = Result<FastStr, ResponseConvertError>> + Send {
        async {
            let bytes = self.into_bytes().await?;

            // SAFETY: The `Vec<u8>` is checked by `simdutf8` and it is a valid `String`
            let _ = simdutf8::basic::from_utf8(&bytes)
                .map_err(|_| ResponseConvertError::StringUtf8Error)?;
            Ok(unsafe { FastStr::from_bytes_unchecked(bytes) })
        }
    }

    /// # Safety
    ///
    /// It is up to the caller to guarantee that the value really is valid. Using this when the
    /// content is invalid causes immediate undefined behavior.
    unsafe fn into_faststr_unchecked(
        self,
    ) -> impl Future<Output = Result<FastStr, ResponseConvertError>> + Send {
        async {
            let bytes = self.into_bytes().await?;

            Ok(FastStr::from_bytes_unchecked(bytes))
        }
    }

    #[cfg(feature = "__json")]
    #[cfg_attr(docsrs, doc(cfg(feature = "json")))]
    fn into_json<T>(self) -> impl Future<Output = Result<T, ResponseConvertError>> + Send
    where
        T: DeserializeOwned,
    {
        async {
            let bytes = self.into_bytes().await?;
            crate::json::deserialize(&bytes).map_err(ResponseConvertError::JsonDeserializeError)
        }
    }
}

impl<T> BodyConversion for T
where
    T: http_body::Body + Send,
    T::Data: Send,
{
}

#[derive(Debug)]
pub enum ResponseConvertError {
    BodyCollectionError,
    StringUtf8Error,
    #[cfg(feature = "__json")]
    #[cfg_attr(docsrs, doc(cfg(feature = "json")))]
    JsonDeserializeError(crate::json::Error),
}

impl fmt::Display for ResponseConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyCollectionError => f.write_str("failed to collect body"),
            Self::StringUtf8Error => f.write_str("body is not a valid string"),
            #[cfg(feature = "__json")]
            Self::JsonDeserializeError(e) => write!(f, "failed to deserialize body: {e}"),
        }
    }
}

impl Error for ResponseConvertError {}

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
