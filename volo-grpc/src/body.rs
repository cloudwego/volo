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
    bytes_stream: BoxStream<'static, Result<Bytes, Status>>,
    state: StreamState,
}

#[derive(Debug)]
enum StreamState {
    Polling,
    Ok,
    ErrorOccurred(Status),
    End,
}

impl Body {
    /// Creates a new [`Body`].
    pub fn new(bytes_stream: BoxStream<'static, Result<Bytes, Status>>) -> Self {
        Self {
            bytes_stream,
            state: StreamState::Polling,
        }
    }

    pub fn end_stream(mut self) -> Self {
        self.state = StreamState::End;
        self
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let mut self_proj = self.project();

        match self_proj.state {
            StreamState::Polling => match ready!(self_proj.bytes_stream.try_poll_next_unpin(cx)) {
                Some(Ok(data)) => Poll::Ready(Some(Ok(Frame::data(data)))),
                Some(Err(err)) => {
                    *self_proj.state = StreamState::ErrorOccurred(err);
                    Poll::Ready(None)
                }
                None => {
                    *self_proj.state = StreamState::Ok;
                    Poll::Ready(None)
                }
            },
            StreamState::Ok => {
                *self_proj.state = StreamState::End;
                let status = Status::new(Code::Ok, "");
                Poll::Ready(Some(Ok(Frame::trailers(status.to_header_map()?))))
            }
            StreamState::ErrorOccurred(status) => {
                let trailer = Frame::trailers(status.to_header_map()?);
                *self_proj.state = StreamState::End;
                Poll::Ready(Some(Ok(trailer)))
            }
            StreamState::End => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self.state, StreamState::End)
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Body").field("state", &self.state).finish()
    }
}
