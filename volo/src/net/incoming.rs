use std::{
    fmt, io,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project::pin_project;
use tokio::net::TcpListener;
#[cfg(target_family = "unix")]
use tokio::net::UnixListener;
#[cfg(target_family = "unix")]
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::{wrappers::TcpListenerStream, StreamExt};

use super::{conn::Conn, Address};

#[pin_project(project = IncomingProj)]
#[derive(Debug)]
pub enum DefaultIncoming {
    Tcp(#[pin] TcpListenerStream),
    #[cfg(target_family = "unix")]
    Unix(#[pin] UnixListenerStream),
}

#[async_trait::async_trait]
impl MakeIncoming for DefaultIncoming {
    type Incoming = DefaultIncoming;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        Ok(self)
    }
}

#[cfg(target_family = "unix")]
impl From<UnixListener> for DefaultIncoming {
    fn from(l: UnixListener) -> Self {
        DefaultIncoming::Unix(UnixListenerStream::new(l))
    }
}

impl From<TcpListener> for DefaultIncoming {
    fn from(l: TcpListener) -> Self {
        DefaultIncoming::Tcp(TcpListenerStream::new(l))
    }
}

#[async_trait::async_trait]
pub trait Incoming: fmt::Debug + Send + 'static {
    async fn accept(&mut self) -> io::Result<Option<Conn>>;
}

#[async_trait::async_trait]
impl Incoming for DefaultIncoming {
    async fn accept(&mut self) -> io::Result<Option<Conn>> {
        if let Some(conn) = self.try_next().await? {
            tracing::trace!("[VOLO] recv a connection from: {:?}", conn.info.peer_addr);
            Ok(Some(conn))
        } else {
            Ok(None)
        }
    }
}

#[async_trait::async_trait]
pub trait MakeIncoming {
    type Incoming: Incoming;

    async fn make_incoming(self) -> io::Result<Self::Incoming>;
}

#[async_trait::async_trait]
impl MakeIncoming for Address {
    type Incoming = DefaultIncoming;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        match self {
            Address::Ip(addr) => TcpListener::bind(addr).await.map(DefaultIncoming::from),
            #[cfg(target_family = "unix")]
            Address::Unix(addr) => UnixListener::bind(addr).map(DefaultIncoming::from),
        }
    }
}

impl Stream for DefaultIncoming {
    type Item = io::Result<Conn>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            IncomingProj::Tcp(s) => s.poll_next(cx).map_ok(Conn::from),
            #[cfg(target_family = "unix")]
            IncomingProj::Unix(s) => s.poll_next(cx).map_ok(Conn::from),
        }
    }
}
