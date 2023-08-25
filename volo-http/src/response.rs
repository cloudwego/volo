use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::{ready, stream};
<<<<<<< HEAD
use http::{Response, StatusCode};
=======
>>>>>>> init
use http_body_util::{Full, StreamBody};
use hyper::body::{Body, Bytes, Frame};
use pin_project_lite::pin_project;

use crate::DynError;

pin_project! {
    #[project = RespBodyProj]
    pub enum RespBody {
        Stream {
            #[pin] inner: StreamBody<stream::Iter<Box<dyn Iterator<Item = Result<Frame<Bytes>, DynError>> + Send + Sync>>>,
        },
        Full {
            #[pin] inner: Full<Bytes>,
        },
    }
}

impl Body for RespBody {
    type Data = Bytes;

    type Error = DynError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.project() {
            RespBodyProj::Stream { inner } => inner.poll_frame(cx),
            RespBodyProj::Full { inner } => {
                Poll::Ready(ready!(inner.poll_frame(cx)).map(|result| Ok(result.unwrap())))
            }
        }
    }
}

impl From<Full<Bytes>> for RespBody {
    fn from(value: Full<Bytes>) -> Self {
        Self::Full { inner: value }
    }
}

impl From<Bytes> for RespBody {
    fn from(value: Bytes) -> Self {
        Self::Full {
            inner: Full::new(value),
        }
    }
}

impl From<String> for RespBody {
    fn from(value: String) -> Self {
        Self::Full {
            inner: Full::new(value.into()),
        }
    }
}

impl From<&'static str> for RespBody {
    fn from(value: &'static str) -> Self {
        Self::Full {
            inner: Full::new(value.into()),
        }
    }
}

impl From<()> for RespBody {
    fn from(_: ()) -> Self {
        Self::Full {
            inner: Full::new(Bytes::new()),
        }
    }
}
<<<<<<< HEAD

pub trait IntoResponse {
    fn into_response(self) -> Response<RespBody>;
}

impl<T> IntoResponse for Response<T>
where
    T: Into<RespBody>,
{
    fn into_response(self) -> Response<RespBody> {
        let (parts, body) = self.into_parts();
        Response::from_parts(parts, body.into())
    }
}

impl<T> IntoResponse for T
where
    T: Into<RespBody>,
{
    fn into_response(self) -> Response<RespBody> {
        Response::builder()
            .status(StatusCode::OK)
            .body(self.into())
            .unwrap()
    }
}

impl<R, E> IntoResponse for Result<R, E>
where
    R: IntoResponse,
    E: IntoResponse,
{
    fn into_response(self) -> Response<RespBody> {
        match self {
            Ok(value) => value.into_response(),
            Err(err) => err.into_response(),
        }
    }
}

impl<T> IntoResponse for (StatusCode, T)
where
    T: IntoResponse,
{
    fn into_response(self) -> Response<RespBody> {
        let mut resp = self.1.into_response();
        *resp.status_mut() = self.0;
        resp
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response<RespBody> {
        Response::builder()
            .status(self)
            .body(String::new().into())
            .unwrap()
    }
}
=======
>>>>>>> init
