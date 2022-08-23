use std::{
    fmt::{self, Formatter},
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{ready, Stream};
use http::HeaderMap;
use hyper::body::HttpBody;
use pin_project::pin_project;

use crate::{status::Code, BoxStream, Status};

/// Similar to [`hyper::Body`], used when sending bodies to client.
///
/// [`Body`] will implement [`HttpBody`] to control the behavior of
/// `poll_data()` and `poll_trailers()`.
#[pin_project]
pub struct Body {
    #[pin]
    bytes_stream: BoxStream<'static, Result<Bytes, Status>>,
    error_occurred: Option<Status>,
    is_end_stream: bool,
}

impl Body {
    /// Creates a new [`Body`].
    pub fn new(bytes_stream: BoxStream<'static, Result<Bytes, Status>>) -> Self {
        Self {
            bytes_stream,
            error_occurred: None,
            is_end_stream: false,
        }
    }

    pub fn status(&self) -> Option<Status> {
        self.error_occurred.clone()
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Status;

    fn is_end_stream(&self) -> bool {
        self.is_end_stream
    }

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let this = self.project();

        // if there is an error, store it and return in poll_trailers().
        match ready!(this.bytes_stream.poll_next(cx)) {
            Some(Ok(data)) => Poll::Ready(Some(Ok(data))),
            Some(Err(err)) => {
                *this.error_occurred = Some(err);
                Poll::Ready(None)
            }
            None => Poll::Ready(None),
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>> {
        let this = self.project();

        // return immediately if there was an error already returned.
        if *this.is_end_stream {
            return Poll::Ready(Ok(None));
        }
        let status = if let Some(status) = this.error_occurred.take() {
            *this.is_end_stream = true;
            status
        } else {
            Status::new(Code::Ok, "")
        };

        Poll::Ready(Ok(Some(status.to_header_map()?)))
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Body")
            .field("error_occurred", &self.error_occurred)
            .field("is_end_stream", &self.is_end_stream)
            .finish()
    }
}
