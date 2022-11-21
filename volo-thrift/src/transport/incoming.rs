use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    ready,
    stream::{Stream, TryStream},
};
use pin_project::pin_project;
use volo::net::conn::Conn;

#[pin_project]
pub struct Incoming {
    #[pin]
    listener: volo::net::incoming::DefaultIncoming,
}

impl Stream for Incoming {
    type Item = Result<Conn, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let mut listener = this.listener;
        match ready!(listener.as_mut().try_poll_next(cx)) {
            Some(Ok(conn)) => Poll::Ready(Some(Ok(conn))),
            Some(Err(e)) => Poll::Ready(Some(Err(e))),
            None => Poll::Ready(None),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test() {}
}
