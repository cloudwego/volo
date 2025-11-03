use std::{
    fmt,
    future::Future,
    io,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use motore::{UnaryService, make::MakeConnection};
use pin_project::pin_project;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};

use super::dial::{Config, MakeTransport};
use crate::net::{
    Address,
    conn::{self, Conn, ConnStream},
};

#[cfg(feature = "native-tls")]
mod native_tls;
#[cfg(feature = "rustls")]
mod rustls;

#[cfg(feature = "native-tls")]
use self::native_tls::{NativeTlsAcceptor, NativeTlsConnector};
#[cfg(feature = "rustls")]
use self::rustls::{RustlsAcceptor, RustlsConnector};

/// A wrapper around [`tokio_rustls::TlsConnector`] and [`tokio_native_tls::TlsConnector`].
#[derive(Clone)]
pub enum TlsConnector {
    #[cfg(feature = "rustls")]
    Rustls(RustlsConnector),
    #[cfg(feature = "native-tls")]
    NativeTls(NativeTlsConnector),
}

/// A wrapper around [`tokio_rustls::TlsAcceptor`] and [`tokio_native_tls::TlsAcceptor`].
#[derive(Clone)]
pub enum TlsAcceptor {
    #[cfg(feature = "rustls")]
    Rustls(RustlsAcceptor),
    #[cfg(feature = "native-tls")]
    NativeTls(NativeTlsAcceptor),
}

#[pin_project(project = TlsStreamProj)]
pub enum TlsStream {
    #[cfg(feature = "rustls")]
    // Since the `tokio_rustls::TlsStream` is too large, it's better to wrap it in `Box`
    Rustls(#[pin] Box<tokio_rustls::TlsStream<TcpStream>>),
    #[cfg(feature = "native-tls")]
    NativeTls(#[pin] Box<tokio_native_tls::TlsStream<TcpStream>>),
}

impl TlsStream {
    pub fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(stream) => stream.get_ref().0.peer_addr(),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(stream) => stream.get_ref().get_ref().get_ref().peer_addr(),
        }
    }

    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(stream) => {
                let (read, write) = tokio::io::split(stream);
                (OwnedReadHalf::Rustls(read), OwnedWriteHalf::Rustls(write))
            }
            #[cfg(feature = "native-tls")]
            Self::NativeTls(stream) => {
                let (read, write) = tokio::io::split(stream);
                (
                    OwnedReadHalf::NativeTls(read),
                    OwnedWriteHalf::NativeTls(write),
                )
            }
        }
    }

    pub fn negotiated_alpn(&self) -> Option<Vec<u8>> {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(stream) => stream.get_ref().1.alpn_protocol().map(ToOwned::to_owned),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(stream) => stream.get_ref().negotiated_alpn().ok().flatten(),
        }
    }
}

#[cfg(feature = "rustls")]
impl From<tokio_rustls::TlsStream<TcpStream>> for TlsStream {
    #[inline]
    fn from(s: tokio_rustls::TlsStream<TcpStream>) -> Self {
        Self::Rustls(Box::new(s))
    }
}

#[cfg(feature = "rustls")]
impl From<Box<tokio_rustls::TlsStream<TcpStream>>> for TlsStream {
    #[inline]
    fn from(s: Box<tokio_rustls::TlsStream<TcpStream>>) -> Self {
        Self::Rustls(s)
    }
}

#[cfg(feature = "native-tls")]
impl From<tokio_native_tls::TlsStream<TcpStream>> for TlsStream {
    #[inline]
    fn from(s: tokio_native_tls::TlsStream<TcpStream>) -> Self {
        Self::NativeTls(Box::new(s))
    }
}

#[cfg(feature = "native-tls")]
impl From<Box<tokio_native_tls::TlsStream<TcpStream>>> for TlsStream {
    #[inline]
    fn from(s: Box<tokio_native_tls::TlsStream<TcpStream>>) -> Self {
        Self::NativeTls(s)
    }
}

impl AsyncRead for TlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            TlsStreamProj::Rustls(stream) => stream.poll_read(cx, buf),
            #[cfg(feature = "native-tls")]
            TlsStreamProj::NativeTls(stream) => stream.poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for TlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            TlsStreamProj::Rustls(stream) => stream.poll_write(cx, buf),
            #[cfg(feature = "native-tls")]
            TlsStreamProj::NativeTls(stream) => stream.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            TlsStreamProj::Rustls(stream) => stream.poll_flush(cx),
            #[cfg(feature = "native-tls")]
            TlsStreamProj::NativeTls(stream) => stream.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            TlsStreamProj::Rustls(stream) => stream.poll_shutdown(cx),
            #[cfg(feature = "native-tls")]
            TlsStreamProj::NativeTls(stream) => stream.poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            TlsStreamProj::Rustls(stream) => stream.poll_write_vectored(cx, bufs),
            #[cfg(feature = "native-tls")]
            TlsStreamProj::NativeTls(stream) => stream.poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(stream) => stream.is_write_vectored(),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(stream) => stream.is_write_vectored(),
        }
    }
}

#[pin_project(project = OwnedWriteHalfProj)]
pub enum OwnedWriteHalf {
    #[cfg(feature = "rustls")]
    Rustls(#[pin] tokio::io::WriteHalf<Box<tokio_rustls::TlsStream<TcpStream>>>),
    #[cfg(feature = "native-tls")]
    NativeTls(#[pin] tokio::io::WriteHalf<Box<tokio_native_tls::TlsStream<TcpStream>>>),
}

impl AsyncWrite for OwnedWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_write(cx, buf),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_flush(cx),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_shutdown(cx),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            OwnedWriteHalfProj::Rustls(half) => half.poll_write_vectored(cx, bufs),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalfProj::NativeTls(half) => half.poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(half) => half.is_write_vectored(),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(half) => half.is_write_vectored(),
        }
    }
}

#[pin_project(project = OwnedReadHalfProj)]
pub enum OwnedReadHalf {
    #[cfg(feature = "rustls")]
    Rustls(#[pin] tokio::io::ReadHalf<Box<tokio_rustls::TlsStream<TcpStream>>>),
    #[cfg(feature = "native-tls")]
    NativeTls(#[pin] tokio::io::ReadHalf<Box<tokio_native_tls::TlsStream<TcpStream>>>),
}

impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            #[cfg(feature = "rustls")]
            OwnedReadHalfProj::Rustls(half) => half.poll_read(cx, buf),
            #[cfg(feature = "native-tls")]
            OwnedReadHalfProj::NativeTls(half) => half.poll_read(cx, buf),
        }
    }
}

trait Connector: Sized {
    fn build(config: TlsConnectorBuilder) -> io::Result<Self>;
    fn connect(
        &self,
        server_name: &str,
        tcp_stream: TcpStream,
    ) -> impl Future<Output = io::Result<TlsStream>> + Send;
}

trait Acceptor: Sized {
    fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> io::Result<Self>;
    fn accept(&self, tcp_stream: TcpStream) -> impl Future<Output = io::Result<TlsStream>> + Send;
}

#[cfg(feature = "rustls")]
impl Default for TlsConnector {
    fn default() -> Self {
        Self::Rustls(RustlsConnector::default())
    }
}

#[cfg(not(feature = "rustls"))]
impl Default for TlsConnector {
    fn default() -> Self {
        Self::NativeTls(NativeTlsConnector::default())
    }
}

impl TlsConnector {
    pub fn builder() -> TlsConnectorBuilder {
        TlsConnectorBuilder::default()
    }

    pub async fn connect(
        &self,
        server_name: &str,
        tcp_stream: TcpStream,
    ) -> io::Result<ConnStream> {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(connector) => connector
                .connect(server_name, tcp_stream)
                .await
                .map(ConnStream::from),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(connector) => connector
                .connect(server_name, tcp_stream)
                .await
                .map(ConnStream::from),
        }
    }
}

impl TlsAcceptor {
    pub async fn accept(&self, tcp_stream: TcpStream) -> io::Result<ConnStream> {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(acceptor) => acceptor.accept(tcp_stream).await.map(ConnStream::from),
            #[cfg(feature = "native-tls")]
            Self::NativeTls(acceptor) => acceptor.accept(tcp_stream).await.map(ConnStream::from),
        }
    }

    #[cfg(feature = "rustls")]
    pub fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> io::Result<Self> {
        RustlsAcceptor::from_pem(cert, key).map(Self::Rustls)
    }

    #[cfg(not(feature = "rustls"))]
    pub fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> io::Result<Self> {
        NativeTlsAcceptor::from_pem(cert, key).map(Self::NativeTls)
    }

    pub fn from_pem_file(
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> io::Result<Self> {
        let cert = std::fs::read(cert_path.as_ref())?;
        let key = std::fs::read(key_path.as_ref())?;
        Self::from_pem(cert, key)
    }
}

impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(_) => f.debug_tuple("TlsConnector::Rustls").finish(),

            #[cfg(feature = "native-tls")]
            Self::NativeTls(_) => f.debug_tuple("TlsConnector::NativeTls").finish(),
        }
    }
}

impl fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(_) => f.debug_tuple("TlsAcceptor::Rustls").finish(),

            #[cfg(feature = "native-tls")]
            Self::NativeTls(_) => f.debug_tuple("TlsAcceptor::NativeTls").finish(),
        }
    }
}

pub struct TlsConnectorBuilder {
    pub(super) default_root_certs: bool,
    pub(super) pems: Vec<Vec<u8>>,
    pub(super) alpn_protocols: Vec<String>,
}

impl Default for TlsConnectorBuilder {
    fn default() -> Self {
        Self {
            default_root_certs: true,
            pems: Vec::new(),
            alpn_protocols: Vec::new(),
        }
    }
}

impl TlsConnectorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enable_default_root_certs(mut self, enable: bool) -> Self {
        self.default_root_certs = enable;
        self
    }

    pub fn add_pem(mut self, cert: Vec<u8>) -> Self {
        self.pems.push(cert);
        self
    }

    pub fn add_pem_from_file<CP>(mut self, cert_path: CP) -> io::Result<Self>
    where
        CP: AsRef<Path>,
    {
        let cert = std::fs::read(cert_path.as_ref())?;
        self.pems.push(cert);
        Ok(self)
    }

    pub fn with_alpn_protocols<I>(mut self, alpn: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.alpn_protocols = alpn.into_iter().map(Into::into).collect();
        self
    }

    #[cfg(feature = "rustls")]
    pub fn build(self) -> io::Result<TlsConnector> {
        Self::build_rustls(self)
    }

    #[cfg(not(feature = "rustls"))]
    pub fn build(self) -> io::Result<TlsConnector> {
        Self::build_native_tls(self)
    }

    #[cfg(feature = "rustls")]
    pub fn build_rustls(self) -> io::Result<TlsConnector> {
        Ok(TlsConnector::Rustls(RustlsConnector::build(self)?))
    }

    #[cfg(feature = "native-tls")]
    pub fn build_native_tls(self) -> io::Result<TlsConnector> {
        Ok(TlsConnector::NativeTls(NativeTlsConnector::build(self)?))
    }
}

/// TLS config for client
#[derive(Debug, Clone)]
pub struct ClientTlsConfig {
    pub server_name: String,
    pub connector: TlsConnector,
}

impl ClientTlsConfig {
    pub fn new(server_name: impl Into<String>, connector: impl Into<TlsConnector>) -> Self {
        Self {
            server_name: server_name.into(),
            connector: connector.into(),
        }
    }
}

/// TLS configuration for a server.
#[derive(Clone)]
pub struct ServerTlsConfig {
    pub acceptor: TlsAcceptor,
}

impl ServerTlsConfig {
    pub fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> io::Result<Self> {
        Ok(Self {
            acceptor: TlsAcceptor::from_pem(cert, key)?,
        })
    }

    pub fn from_pem_file<CP, KP>(cert_path: CP, key_path: KP) -> io::Result<Self>
    where
        CP: AsRef<std::path::Path>,
        KP: AsRef<std::path::Path>,
    {
        let cert = std::fs::read(cert_path.as_ref())?;
        let key = std::fs::read(key_path.as_ref())?;
        Self::from_pem(cert, key)
    }
}

#[derive(Debug, Clone)]
pub struct TlsMakeTransport {
    cfg: Config,
    tls_config: ClientTlsConfig,
}

impl TlsMakeTransport {
    pub fn new(cfg: Config, tls_config: ClientTlsConfig) -> Self {
        Self { cfg, tls_config }
    }
}

impl UnaryService<Address> for TlsMakeTransport {
    type Response = Conn;
    type Error = io::Error;

    async fn call(&self, addr: Address) -> std::result::Result<Self::Response, Self::Error> {
        match addr {
            Address::Ip(addr) => {
                let tcp = super::dial::make_tcp_connection(&self.cfg, addr).await?;

                match &self.tls_config.connector {
                    #[cfg(feature = "rustls")]
                    TlsConnector::Rustls(connector) => connector
                        .connect(&self.tls_config.server_name, tcp)
                        .await
                        .map(Conn::from),
                    #[cfg(feature = "native-tls")]
                    TlsConnector::NativeTls(connector) => connector
                        .connect(&self.tls_config.server_name, tcp)
                        .await
                        .map(Conn::from),
                }
            }
            #[cfg(target_family = "unix")]
            Address::Unix(_) => Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "unix domain socket is unavailable for tls",
            )),
        }
    }
}

impl MakeTransport for TlsMakeTransport {
    type ReadHalf = conn::OwnedReadHalf;
    type WriteHalf = conn::OwnedWriteHalf;

    async fn make_transport(&self, addr: Address) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
        let conn = self.make_connection(addr).await?;
        let (read, write) = conn.stream.into_split();
        Ok((read, write))
    }

    fn set_connect_timeout(&mut self, timeout: Option<Duration>) {
        self.cfg = self.cfg.with_connect_timeout(timeout);
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.cfg = self.cfg.with_read_timeout(timeout);
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) {
        self.cfg = self.cfg.with_write_timeout(timeout);
    }
}
