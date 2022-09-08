use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{tcp, TcpStream},
};

#[cfg(not(target_os = "windows"))]
use tokio::net::{unix, UnixStream};

use super::Address;

#[derive(Clone)]
pub struct ConnInfo {
    pub peer_addr: Option<Address>,
}

pub trait DynStream: AsyncRead + AsyncWrite + Send + 'static {}

impl<T> DynStream for T where T: AsyncRead + AsyncWrite + Send + 'static {}

#[cfg(target_os = "windows")]
#[pin_project(project = IoStreamProj)]
pub enum ConnStream {
    Tcp(#[pin] TcpStream),
}

#[cfg(not(target_os = "windows"))]
#[pin_project(project = IoStreamProj)]
pub enum ConnStream {
    Tcp(#[pin] TcpStream),
    Unix(#[pin] UnixStream),
}

#[cfg(target_os = "windows")]
#[pin_project(project = OwnedWriteHalfProj)]
pub enum OwnedWriteHalf {
    Tcp(#[pin] tcp::OwnedWriteHalf),
}

#[cfg(not(target_os = "windows"))]
#[pin_project(project = OwnedWriteHalfProj)]
pub enum OwnedWriteHalf {
    Tcp(#[pin] tcp::OwnedWriteHalf),
    Unix(#[pin] unix::OwnedWriteHalf),
}

#[cfg(target_os = "windows")]
impl AsyncWrite for OwnedWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_flush(cx),
        }

    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_shutdown(cx),
        }

    }
}

#[cfg(not(target_os = "windows"))]
impl AsyncWrite for OwnedWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_write(cx, buf),
            OwnedWriteHalfProj::Unix(half) => half.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_flush(cx),
            OwnedWriteHalfProj::Unix(half) => half.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_shutdown(cx),
            OwnedWriteHalfProj::Unix(half) => half.poll_shutdown(cx),
        }
    }
}

#[cfg(target_os = "windows")]
#[pin_project(project = OwnedReadHalfProj)]
pub enum OwnedReadHalf {
    Tcp(#[pin] tcp::OwnedReadHalf),
}

#[cfg(not(target_os = "windows"))]
#[pin_project(project = OwnedReadHalfProj)]
pub enum OwnedReadHalf {
    Tcp(#[pin] tcp::OwnedReadHalf),
    Unix(#[pin] unix::OwnedReadHalf),
}

#[cfg(target_os = "windows")]
impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedReadHalfProj::Tcp(half) => half.poll_read(cx, buf),
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedReadHalfProj::Tcp(half) => half.poll_read(cx, buf),
            OwnedReadHalfProj::Unix(half) => half.poll_read(cx, buf),
        }
    }
}

#[cfg(target_os = "windows")]
impl ConnStream {
    #[allow(clippy::type_complexity)]
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        match self {
            ConnStream::Tcp(stream) => {
                let (rh, wh) = stream.into_split();
                (OwnedReadHalf::Tcp(rh), OwnedWriteHalf::Tcp(wh))
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl ConnStream {
    #[allow(clippy::type_complexity)]
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        match self {
            ConnStream::Tcp(stream) => {
                let (rh, wh) = stream.into_split();
                (OwnedReadHalf::Tcp(rh), OwnedWriteHalf::Tcp(wh))
            }
            ConnStream::Unix(stream) => {
                let (rh, wh) = stream.into_split();
                (OwnedReadHalf::Unix(rh), OwnedWriteHalf::Unix(wh))
            }
        }
    }
}

impl From<TcpStream> for ConnStream {
    #[inline]
    fn from(s: TcpStream) -> Self {
        let _ = s.set_nodelay(true);
        ConnStream::Tcp(s)
    }
}

#[cfg(not(target_os = "windows"))]
impl From<UnixStream> for ConnStream {
    #[inline]
    fn from(s: UnixStream) -> Self {
        ConnStream::Unix(s)
    }
}

#[cfg(target_os = "windows")]
impl AsyncRead for ConnStream {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_read(cx, buf),
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl AsyncRead for ConnStream {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_read(cx, buf),
            IoStreamProj::Unix(s) => s.poll_read(cx, buf),
        }
    }
}

#[cfg(target_os = "windows")]
impl AsyncWrite for ConnStream {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_write(cx, buf),
        }

    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_shutdown(cx),
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl AsyncWrite for ConnStream {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_write(cx, buf),
            IoStreamProj::Unix(s) => s.poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_flush(cx),
            IoStreamProj::Unix(s) => s.poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_shutdown(cx),
            IoStreamProj::Unix(s) => s.poll_shutdown(cx),
        }
    }
}

#[cfg(target_os = "windows")]
impl ConnStream {
    #[inline]
    pub fn peer_addr(&self) -> Option<Address> {
        match self {
            ConnStream::Tcp(s) => s.peer_addr().map(Address::from).ok(),
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl ConnStream {
    #[inline]
    pub fn peer_addr(&self) -> Option<Address> {
        match self {
            ConnStream::Tcp(s) => s.peer_addr().map(Address::from).ok(),
            ConnStream::Unix(s) => s.peer_addr().ok().and_then(|s| Address::try_from(s).ok()),
        }
    }
}
pub struct Conn {
    pub stream: ConnStream,
    pub info: ConnInfo,
}

impl Conn {
    pub fn new(stream: ConnStream, info: ConnInfo) -> Self {
        Conn { stream, info }
    }
}

impl<T> From<T> for Conn
where
    T: Into<ConnStream>,
{
    #[inline]
    fn from(i: T) -> Self {
        let i = i.into();
        let peer_addr = i.peer_addr();
        Conn::new(i, ConnInfo { peer_addr })
    }
}

impl AsyncRead for Conn {
    #[inline]
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Conn {
    #[inline]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    #[inline]
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    #[inline]
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
