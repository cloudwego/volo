use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;
#[cfg(target_family = "unix")]
use tokio::net::{UnixStream, unix};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpStream, tcp},
};

use super::Address;

#[derive(Clone)]
pub struct ConnInfo {
    pub peer_addr: Option<Address>,
}

#[pin_project(project = IoStreamProj)]
pub enum ConnStream {
    Tcp(#[pin] TcpStream),
    #[cfg(target_family = "unix")]
    Unix(#[pin] UnixStream),
    #[cfg(feature = "__tls")]
    Tls(#[pin] super::tls::TlsStream),
    #[cfg(feature = "shmipc")]
    Shmipc(#[pin] super::shmipc::Stream),
}

impl ConnStream {
    pub fn is_tcp(&self) -> bool {
        matches!(self, Self::Tcp(_))
    }

    #[cfg(target_family = "unix")]
    pub fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }

    #[cfg(feature = "__tls")]
    pub fn is_tls(&self) -> bool {
        matches!(self, Self::Tls(_))
    }

    #[cfg(feature = "shmipc")]
    pub fn is_shmipc(&self) -> bool {
        matches!(self, Self::Shmipc(_))
    }

    pub fn into_tcp(self) -> Option<TcpStream> {
        match self {
            Self::Tcp(stream) => Some(stream),
            #[cfg(target_family = "unix")]
            Self::Unix(_) => None,
            #[cfg(feature = "__tls")]
            Self::Tls(_) => None,
            #[cfg(feature = "shmipc")]
            Self::Shmipc(_) => None,
        }
    }

    #[cfg(target_family = "unix")]
    pub fn into_unix(self) -> Option<UnixStream> {
        match self {
            Self::Unix(stream) => Some(stream),
            _ => None,
        }
    }

    #[cfg(feature = "__tls")]
    pub fn into_tls(self) -> Option<super::tls::TlsStream> {
        match self {
            Self::Tls(stream) => Some(stream),
            _ => None,
        }
    }

    #[cfg(feature = "shmipc")]
    pub fn into_shmipc(self) -> Option<super::shmipc::Stream> {
        match self {
            Self::Shmipc(stream) => Some(stream),
            _ => None,
        }
    }
}

#[pin_project(project = OwnedWriteHalfProj)]
pub enum OwnedWriteHalf {
    Tcp(#[pin] tcp::OwnedWriteHalf),
    #[cfg(target_family = "unix")]
    Unix(#[pin] unix::OwnedWriteHalf),
    #[cfg(feature = "__tls")]
    Tls(#[pin] super::tls::OwnedWriteHalf),
    #[cfg(feature = "shmipc")]
    Shmipc(#[pin] super::shmipc::WriteHalf),
}

impl AsyncWrite for OwnedWriteHalf {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_write(cx, buf),
            #[cfg(target_family = "unix")]
            OwnedWriteHalfProj::Unix(half) => half.poll_write(cx, buf),
            #[cfg(feature = "__tls")]
            OwnedWriteHalfProj::Tls(half) => half.poll_write(cx, buf),
            #[cfg(feature = "shmipc")]
            OwnedWriteHalfProj::Shmipc(half) => half.poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_flush(cx),
            #[cfg(target_family = "unix")]
            OwnedWriteHalfProj::Unix(half) => half.poll_flush(cx),
            #[cfg(feature = "__tls")]
            OwnedWriteHalfProj::Tls(half) => half.poll_flush(cx),
            #[cfg(feature = "shmipc")]
            OwnedWriteHalfProj::Shmipc(half) => half.poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_shutdown(cx),
            #[cfg(target_family = "unix")]
            OwnedWriteHalfProj::Unix(half) => half.poll_shutdown(cx),
            #[cfg(feature = "__tls")]
            OwnedWriteHalfProj::Tls(half) => half.poll_shutdown(cx),
            #[cfg(feature = "shmipc")]
            OwnedWriteHalfProj::Shmipc(half) => half.poll_shutdown(cx),
        }
    }

    #[inline]
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_write_vectored(cx, bufs),
            #[cfg(target_family = "unix")]
            OwnedWriteHalfProj::Unix(half) => half.poll_write_vectored(cx, bufs),
            #[cfg(feature = "__tls")]
            OwnedWriteHalfProj::Tls(half) => half.poll_write_vectored(cx, bufs),
            #[cfg(feature = "shmipc")]
            OwnedWriteHalfProj::Shmipc(half) => half.poll_write_vectored(cx, bufs),
        }
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tcp(half) => half.is_write_vectored(),
            #[cfg(target_family = "unix")]
            Self::Unix(half) => half.is_write_vectored(),
            #[cfg(feature = "__tls")]
            Self::Tls(half) => half.is_write_vectored(),
            #[cfg(feature = "shmipc")]
            Self::Shmipc(half) => half.is_write_vectored(),
        }
    }
}

#[pin_project(project = OwnedReadHalfProj)]
pub enum OwnedReadHalf {
    Tcp(#[pin] tcp::OwnedReadHalf),
    #[cfg(target_family = "unix")]
    Unix(#[pin] unix::OwnedReadHalf),
    #[cfg(feature = "__tls")]
    Tls(#[pin] super::tls::OwnedReadHalf),
    #[cfg(feature = "shmipc")]
    Shmipc(#[pin] super::shmipc::ReadHalf),
}

impl OwnedReadHalf {
    #[cfg(feature = "shmipc")]
    pub fn shmipc_helper(&self) -> super::shmipc::ShmipcHelper {
        match self {
            Self::Shmipc(rh) => rh.helper(),
            _ => Default::default(),
        }
    }
}

impl AsyncRead for OwnedReadHalf {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedReadHalfProj::Tcp(half) => half.poll_read(cx, buf),
            #[cfg(target_family = "unix")]
            OwnedReadHalfProj::Unix(half) => half.poll_read(cx, buf),
            #[cfg(feature = "__tls")]
            OwnedReadHalfProj::Tls(half) => half.poll_read(cx, buf),
            #[cfg(feature = "shmipc")]
            OwnedReadHalfProj::Shmipc(half) => half.poll_read(cx, buf),
        }
    }
}

impl ConnStream {
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        match self {
            Self::Tcp(stream) => {
                let (rh, wh) = stream.into_split();
                (OwnedReadHalf::Tcp(rh), OwnedWriteHalf::Tcp(wh))
            }
            #[cfg(target_family = "unix")]
            Self::Unix(stream) => {
                let (rh, wh) = stream.into_split();
                (OwnedReadHalf::Unix(rh), OwnedWriteHalf::Unix(wh))
            }
            #[cfg(feature = "__tls")]
            Self::Tls(stream) => {
                let (rh, wh) = stream.into_split();
                (OwnedReadHalf::Tls(rh), OwnedWriteHalf::Tls(wh))
            }
            #[cfg(feature = "shmipc")]
            Self::Shmipc(stream) => {
                let (rh, wh) = stream.into_split();
                (OwnedReadHalf::Shmipc(rh), OwnedWriteHalf::Shmipc(wh))
            }
        }
    }

    pub fn negotiated_alpn(&self) -> Option<Vec<u8>> {
        match self {
            #[cfg(feature = "__tls")]
            Self::Tls(stream) => stream.negotiated_alpn(),
            _ => None,
        }
    }
}

impl From<TcpStream> for ConnStream {
    #[inline]
    fn from(s: TcpStream) -> Self {
        let _ = s.set_nodelay(true);
        Self::Tcp(s)
    }
}

#[cfg(target_family = "unix")]
impl From<UnixStream> for ConnStream {
    #[inline]
    fn from(s: UnixStream) -> Self {
        Self::Unix(s)
    }
}

#[cfg(feature = "__tls")]
impl<T> From<T> for ConnStream
where
    T: Into<super::tls::TlsStream>,
{
    #[inline]
    fn from(s: T) -> Self {
        Self::Tls(s.into())
    }
}

#[cfg(feature = "shmipc")]
impl From<super::shmipc::Stream> for ConnStream {
    #[inline]
    fn from(value: super::shmipc::Stream) -> Self {
        Self::Shmipc(value)
    }
}

impl AsyncRead for ConnStream {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_read(cx, buf),
            #[cfg(target_family = "unix")]
            IoStreamProj::Unix(s) => s.poll_read(cx, buf),
            #[cfg(feature = "__tls")]
            IoStreamProj::Tls(s) => s.poll_read(cx, buf),
            #[cfg(feature = "shmipc")]
            IoStreamProj::Shmipc(s) => s.poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ConnStream {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_write(cx, buf),
            #[cfg(target_family = "unix")]
            IoStreamProj::Unix(s) => s.poll_write(cx, buf),
            #[cfg(feature = "__tls")]
            IoStreamProj::Tls(s) => s.poll_write(cx, buf),
            #[cfg(feature = "shmipc")]
            IoStreamProj::Shmipc(s) => s.poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_flush(cx),
            #[cfg(target_family = "unix")]
            IoStreamProj::Unix(s) => s.poll_flush(cx),
            #[cfg(feature = "__tls")]
            IoStreamProj::Tls(s) => s.poll_flush(cx),
            #[cfg(feature = "shmipc")]
            IoStreamProj::Shmipc(s) => s.poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_shutdown(cx),
            #[cfg(target_family = "unix")]
            IoStreamProj::Unix(s) => s.poll_shutdown(cx),
            #[cfg(feature = "__tls")]
            IoStreamProj::Tls(s) => s.poll_shutdown(cx),
            #[cfg(feature = "shmipc")]
            IoStreamProj::Shmipc(s) => s.poll_shutdown(cx),
        }
    }

    #[inline]
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_write_vectored(cx, bufs),
            #[cfg(target_family = "unix")]
            IoStreamProj::Unix(s) => s.poll_write_vectored(cx, bufs),
            #[cfg(feature = "__tls")]
            IoStreamProj::Tls(s) => s.poll_write_vectored(cx, bufs),
            #[cfg(feature = "shmipc")]
            IoStreamProj::Shmipc(s) => s.poll_write_vectored(cx, bufs),
        }
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tcp(s) => s.is_write_vectored(),
            #[cfg(target_family = "unix")]
            Self::Unix(s) => s.is_write_vectored(),
            #[cfg(feature = "__tls")]
            Self::Tls(s) => s.is_write_vectored(),
            #[cfg(feature = "shmipc")]
            Self::Shmipc(s) => s.is_write_vectored(),
        }
    }
}

impl ConnStream {
    #[inline]
    pub fn peer_addr(&self) -> Option<Address> {
        match self {
            Self::Tcp(s) => s.peer_addr().map(Address::from).ok(),
            #[cfg(target_family = "unix")]
            Self::Unix(s) => s.peer_addr().map(Address::from).ok(),
            #[cfg(feature = "__tls")]
            Self::Tls(s) => s.peer_addr().map(Address::from).ok(),
            #[cfg(feature = "shmipc")]
            Self::Shmipc(s) => Some(Address::from(s.peer_addr())),
        }
    }
}

pub struct Conn {
    pub stream: ConnStream,
    pub info: ConnInfo,
}

impl Conn {
    #[inline]
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

    #[inline]
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.stream).poll_write_vectored(cx, bufs)
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}
