use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use faststr::FastStr;
use futures_util::ready;
use http_body::{Frame, SizeHint};
use http_body_util::Full;
use pin_project::pin_project;

#[pin_project]
pub struct Body {
    #[pin]
    inner: Full<Bytes>,
}

impl http_body::Body for Body {
    type Data = Bytes;

    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(ready!(self.project().inner.poll_frame(cx)).map(|result| Ok(result.unwrap())))
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl From<()> for Body {
    fn from(_: ()) -> Self {
        Self {
            inner: Full::new(Bytes::new()),
        }
    }
}

impl From<&'static str> for Body {
    fn from(value: &'static str) -> Self {
        Self {
            inner: Full::new(value.into()),
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(value: Vec<u8>) -> Self {
        Self {
            inner: Full::new(value.into()),
        }
    }
}

impl From<Bytes> for Body {
    fn from(value: Bytes) -> Self {
        Self {
            inner: Full::new(value),
        }
    }
}

impl From<FastStr> for Body {
    fn from(value: FastStr) -> Self {
        Self {
            inner: Full::new(value.into()),
        }
    }
}

impl From<String> for Body {
    fn from(value: String) -> Self {
        Self {
            inner: Full::new(value.into()),
        }
    }
}

impl From<Full<Bytes>> for Body {
    fn from(value: Full<Bytes>) -> Self {
        Self { inner: value }
    }
}
