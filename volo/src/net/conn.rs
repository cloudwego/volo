use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;
#[cfg(target_family = "unix")]
use tokio::net::{unix, UnixStream};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{tcp, TcpStream},
};

use super::Address;

#[derive(Clone)]
pub struct ConnInfo {
    pub peer_addr: Option<Address>,
}

pub trait DynStream: AsyncRead + AsyncWrite + Send + 'static {}

impl<T> DynStream for T where T: AsyncRead + AsyncWrite + Send + 'static {}

#[pin_project(project = IoStreamProj)]
pub enum ConnStream {
    Tcp(#[pin] TcpStream),
    #[cfg(target_family = "unix")]
    Unix(#[pin] UnixStream),
    #[cfg(feature = "rustls")]
    Rustls(#[pin] tokio_rustls::TlsStream<TcpStream>),
    #[cfg(feature = "native-tls")]
    NativeTls(#[pin] tokio_native_tls::TlsStream<TcpStream>),
}

cfg_rustls! {
    type RustlsWriteHalf = tokio::io::WriteHalf<tokio_rustls::TlsStream<TcpStream>>;
}

cfg_native_tls! {
    type NativeTlsWriteHalf = tokio::io::WriteHalf<tokio_native_tls::TlsStream<TcpStream>>;
}

#[pin_project(project = OwnedWriteHalfProj)]
pub enum OwnedWriteHalf {
    Tcp(#[pin] tcp::OwnedWriteHalf),
    #[cfg(target_family = "unix")]
    Unix(#[pin] unix::OwnedWriteHalf),
    #[cfg(feature = "rustls")]
    Rustls(#[pin] RustlsWriteHalf),
    #[cfg(feature = "native-tls")]
    NativeTls(#[pin] NativeTlsWriteHalf),
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
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_write(cx, buf),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_flush(cx),
            #[cfg(target_family = "unix")]
            OwnedWriteHalfProj::Unix(half) => half.poll_flush(cx),
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_flush(cx),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            OwnedWriteHalfProj::Tcp(half) => half.poll_shutdown(cx),
            #[cfg(target_family = "unix")]
            OwnedWriteHalfProj::Unix(half) => half.poll_shutdown(cx),
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_shutdown(cx),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_shutdown(cx),
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
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_write_vectored(cx, bufs),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_write_vectored(cx, bufs),
        }
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tcp(half) => half.is_write_vectored(),
            #[cfg(target_family = "unix")]
            Self::Unix(half) => half.is_write_vectored(),
            #[cfg(feature = "rustls")]
            Self::Rustls(half) => half.is_write_vectored(),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(half) => half.is_write_vectored(),
        }
    }
}

cfg_rustls! {
    type RustlsReadHalf = tokio::io::ReadHalf<tokio_rustls::TlsStream<TcpStream>>;
}

cfg_native_tls! {
    type NativeTlsReadHalf = tokio::io::ReadHalf<tokio_native_tls::TlsStream<TcpStream>>;
}

#[pin_project(project = OwnedReadHalfProj)]
pub enum OwnedReadHalf {
    Tcp(#[pin] tcp::OwnedReadHalf),
    #[cfg(target_family = "unix")]
    Unix(#[pin] unix::OwnedReadHalf),
    #[cfg(feature = "rustls")]
    Rustls(#[pin] RustlsReadHalf),
    #[cfg(feature = "native-tls")]
    NativeTls(#[pin] NativeTlsReadHalf),
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
            #[cfg(feature = "rustls")]
            OwnedReadHalfProj::Rustls(half) => half.poll_read(cx, buf),
            #[cfg(feature = "native-tls")]
            OwnedReadHalfProj::NativeTls(half) => half.poll_read(cx, buf),
        }
    }
}

impl ConnStream {
    #[allow(clippy::type_complexity)]
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
            #[cfg(feature = "rustls")]
            Self::Rustls(stream) => {
                let (rh, wh) = tokio::io::split(stream);
                (OwnedReadHalf::Rustls(rh), OwnedWriteHalf::Rustls(wh))
            }
            #[cfg(feature = "native-tls")]
            Self::NativeTls(stream) => {
                let (rh, wh) = tokio::io::split(stream);
                (OwnedReadHalf::NativeTls(rh), OwnedWriteHalf::NativeTls(wh))
            }
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

cfg_rustls! {
    impl From<tokio_rustls::TlsStream<TcpStream>> for ConnStream {
        #[inline]
        fn from(s: tokio_rustls::TlsStream<TcpStream>) -> Self {
            Self::Rustls(s)
        }
    }
}

cfg_native_tls! {
    impl From<tokio_native_tls::TlsStream<TcpStream>> for ConnStream {
        #[inline]
        fn from(s: tokio_native_tls::TlsStream<TcpStream>) -> Self {
            Self::NativeTls(s)
        }
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
            #[cfg(feature = "rustls")]
            IoStreamProj::Rustls(s) => s.poll_read(cx, buf),
            #[cfg(feature = "native-tls")]
            IoStreamProj::NativeTls(s) => s.poll_read(cx, buf),
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
            #[cfg(feature = "rustls")]
            IoStreamProj::Rustls(s) => s.poll_write(cx, buf),
            #[cfg(feature = "native-tls")]
            IoStreamProj::NativeTls(s) => s.poll_write(cx, buf),
        }
    }

    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_flush(cx),
            #[cfg(target_family = "unix")]
            IoStreamProj::Unix(s) => s.poll_flush(cx),
            #[cfg(feature = "rustls")]
            IoStreamProj::Rustls(s) => s.poll_flush(cx),
            #[cfg(feature = "native-tls")]
            IoStreamProj::NativeTls(s) => s.poll_flush(cx),
        }
    }

    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            IoStreamProj::Tcp(s) => s.poll_shutdown(cx),
            #[cfg(target_family = "unix")]
            IoStreamProj::Unix(s) => s.poll_shutdown(cx),
            #[cfg(feature = "rustls")]
            IoStreamProj::Rustls(s) => s.poll_shutdown(cx),
            #[cfg(feature = "native-tls")]
            IoStreamProj::NativeTls(s) => s.poll_shutdown(cx),
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
            #[cfg(feature = "rustls")]
            IoStreamProj::Rustls(s) => s.poll_write_vectored(cx, bufs),
            #[cfg(feature = "native-tls")]
            IoStreamProj::NativeTls(s) => s.poll_write_vectored(cx, bufs),
        }
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tcp(s) => s.is_write_vectored(),
            #[cfg(target_family = "unix")]
            Self::Unix(s) => s.is_write_vectored(),
            #[cfg(feature = "rustls")]
            Self::Rustls(s) => s.is_write_vectored(),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(s) => s.is_write_vectored(),
        }
    }
}

impl ConnStream {
    #[inline]
    pub fn peer_addr(&self) -> Option<Address> {
        match self {
            Self::Tcp(s) => s.peer_addr().map(Address::from).ok(),
            #[cfg(target_family = "unix")]
            Self::Unix(s) => s.peer_addr().ok().and_then(|s| Address::try_from(s).ok()),
            #[cfg(feature = "rustls")]
            Self::Rustls(s) => s.get_ref().0.peer_addr().map(Address::from).ok(),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(s) => s
                .get_ref()
                .get_ref()
                .get_ref()
                .peer_addr()
                .map(Address::from)
                .ok(),
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
