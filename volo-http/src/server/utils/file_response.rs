use std::{
    fs::File,
    io,
    path::Path,
    pin::Pin,
    task::{ready, Context, Poll},
};

use bytes::Bytes;
use futures::Stream;
use http::header::{self, HeaderValue};
use http_body::{Frame, SizeHint};
use pin_project::pin_project;
use tokio::io::AsyncRead;
use tokio_util::io::ReaderStream;

use crate::{body::Body, response::ServerResponse, server::IntoResponse};

const BUF_SIZE: usize = 4096;

pub struct FileResponse {
    file: File,
    size: u64,
    content_type: HeaderValue,
}

impl FileResponse {
    pub fn new<P>(path: P, content_type: HeaderValue) -> io::Result<Self>
    where
        P: AsRef<Path>,
    {
        let file = File::open(path)?;
        let metadata = file.metadata()?;

        Ok(Self {
            file,
            size: metadata.len(),
            content_type,
        })
    }
}

impl IntoResponse for FileResponse {
    fn into_response(self) -> ServerResponse {
        let file = tokio::fs::File::from_std(self.file);
        ServerResponse::builder()
            .header(header::CONTENT_TYPE, self.content_type)
            .body(Body::from_body(FileBody {
                reader: ReaderStream::with_capacity(file, BUF_SIZE),
                size: self.size,
            }))
            .unwrap()
    }
}

#[pin_project]
struct FileBody<R> {
    #[pin]
    reader: ReaderStream<R>,
    size: u64,
}

impl<R> http_body::Body for FileBody<R>
where
    R: AsyncRead,
{
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match ready!(self.project().reader.poll_next(cx)) {
            Some(Ok(chunk)) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
            Some(Err(err)) => Poll::Ready(Some(Err(err))),
            None => Poll::Ready(None),
        }
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.size)
    }
}
