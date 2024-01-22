use std::{
    fmt::{self, Formatter},
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{ready, TryStreamExt};
use http_body::{Body as HttpBody, Frame};
use pin_project::pin_project;

use crate::{BoxStream, Code, Status};

/// Similar to [`hyper::Body`], used when sending bodies to client.
///
/// [`Body`] will implement [`HttpBody`] to control the behavior of
/// `poll_data()` and `poll_trailers()`.
#[pin_project]
pub struct Body {
    #[pin]
    bytes_stream: BoxStream<'static, Result<Frame<Bytes>, Status>>,
    is_end_stream: bool,
}

impl Body {
    /// Creates a new [`Body`].
    pub fn new(bytes_stream: BoxStream<'static, Result<Frame<Bytes>, Status>>) -> Self {
        Self {
            bytes_stream,
            is_end_stream: false,
        }
    }

    pub fn end_stream(mut self) -> Self {
        self.is_end_stream = true;
        self
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let mut self_proj = self.project();

        if !*self_proj.is_end_stream {
            match ready!(self_proj.bytes_stream.try_poll_next_unpin(cx)) {
                Some(Ok(data)) => Poll::Ready(Some(Ok(data))),
                Some(Err(status)) => {
                    tracing::debug!("[VOLO] failed to poll stream");
                    *self_proj.is_end_stream = true;
                    Poll::Ready(Some(Ok(Frame::trailers(status.to_header_map()?))))
                }
                None => {
                    *self_proj.is_end_stream = true;
                    Poll::Ready(Some(Ok(Frame::trailers(
                        Status::new(Code::Ok, "").to_header_map()?,
                    ))))
                }
            }
        } else {
            Poll::Ready(None)
        }
    }

    fn is_end_stream(&self) -> bool {
        self.is_end_stream
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Body")
            .field("is_end_stream", &self.is_end_stream)
            .finish()
    }
}
