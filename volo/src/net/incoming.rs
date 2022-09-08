use std::{
    io,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project::pin_project;
use tokio::net::{TcpListener};
use tokio_stream::wrappers::{TcpListenerStream};

#[cfg(not(target_os = "windows"))]
use tokio::net::UnixListener;

#[cfg(not(target_os = "windows"))]
use tokio_stream::wrappers::UnixListenerStream;

use super::{conn::Conn, Address};

#[cfg(not(target_os = "windows"))]
#[pin_project(project = IncomingProj)]
#[derive(Debug)]
pub enum Incoming {
    Tcp(#[pin] TcpListenerStream),
    Unix(#[pin] UnixListenerStream),
}

#[cfg(target_os = "windows")]
#[pin_project(project = IncomingProj)]
#[derive(Debug)]
pub enum Incoming {
    Tcp(#[pin] TcpListenerStream),
}

#[async_trait::async_trait]
impl MakeIncoming for Incoming {
    async fn make_incoming(self) -> Result<Incoming, std::io::Error> {
        Ok(self)
    }
}

#[cfg(not(target_os = "windows"))]
impl From<UnixListener> for Incoming {
    fn from(l: UnixListener) -> Self {
        Incoming::Unix(UnixListenerStream::new(l))
    }
}

impl From<TcpListener> for Incoming {
    fn from(l: TcpListener) -> Self {
        Incoming::Tcp(TcpListenerStream::new(l))
    }
}

#[async_trait::async_trait]
pub trait MakeIncoming {
    async fn make_incoming(self) -> Result<Incoming, std::io::Error>;
}

#[cfg(target_os = "windows")]
#[async_trait::async_trait]
impl MakeIncoming for Address {
    async fn make_incoming(self) -> Result<Incoming, std::io::Error> {
        match self {
            Address::Ip(addr) => TcpListener::bind(addr).await.map(Incoming::from),
        }
    }
}

#[cfg(not(target_os = "windows"))]
#[async_trait::async_trait]
impl MakeIncoming for Address {
    async fn make_incoming(self) -> Result<Incoming, std::io::Error> {
        match self {
            Address::Ip(addr) => TcpListener::bind(addr).await.map(Incoming::from),
            Address::Unix(addr) => UnixListener::bind(addr).map(Incoming::from),
        }
    }
}

#[cfg(target_os = "windows")]
impl Stream for Incoming {
    type Item = io::Result<Conn>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            IncomingProj::Tcp(s) => s.poll_next(cx).map_ok(Conn::from),
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl Stream for Incoming {
    type Item = io::Result<Conn>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            IncomingProj::Tcp(s) => s.poll_next(cx).map_ok(Conn::from),
            IncomingProj::Unix(s) => s.poll_next(cx).map_ok(Conn::from),
        }
    }
}