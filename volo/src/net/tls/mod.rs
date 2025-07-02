use std::{
    fmt,
    future::Future,
    io::{self, Result},
    path::Path,
    time::Duration,
};

use motore::{UnaryService, make::MakeConnection};
use tokio::net::TcpStream;
#[cfg(target_family = "unix")]
use tokio::net::UnixStream;

use super::{
    conn::ConnStream,
    dial::{Config, MakeTransport, make_tcp_connection},
};
use crate::net::{
    Address,
    conn::{Conn, OwnedReadHalf, OwnedWriteHalf},
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

pub trait Connector: Sized {
    fn build(config: TlsConnectorBuilder) -> Result<Self>;
    fn connect(
        &self,
        server_name: &str,
        tcp_stream: TcpStream,
    ) -> impl Future<Output = Result<Conn>> + Send;
}

pub trait Acceptor: Sized {
    fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> Result<Self>;
    fn from_pem_file(cert_path: impl AsRef<Path>, key_path: impl AsRef<Path>) -> Result<Self>;
    fn accept(&self, tcp_stream: TcpStream) -> impl Future<Output = Result<ConnStream>> + Send;
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
}

impl Connector for TlsConnector {
    fn build(builder: TlsConnectorBuilder) -> Result<Self> {
        builder.build()
    }

    async fn connect(&self, server_name: &str, tcp_stream: TcpStream) -> Result<Conn> {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(connector) => connector.connect(server_name, tcp_stream).await,

            #[cfg(feature = "native-tls")]
            Self::NativeTls(connector) => connector.connect(server_name, tcp_stream).await,
        }
    }
}

impl Acceptor for TlsAcceptor {
    async fn accept(&self, tcp_stream: TcpStream) -> Result<ConnStream> {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(acceptor) => acceptor.accept(tcp_stream).await,

            #[cfg(feature = "native-tls")]
            Self::NativeTls(acceptor) => acceptor.accept(tcp_stream).await,
        }
    }

    #[cfg(feature = "rustls")]
    fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> Result<Self> {
        Ok(Self::Rustls(RustlsAcceptor::from_pem(cert, key)?))
    }

    #[cfg(not(feature = "rustls"))]
    fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> Result<Self> {
        Ok(Self::NativeTls(NativeTlsAcceptor::from_pem(cert, key)?))
    }

    fn from_pem_file(cert_path: impl AsRef<Path>, key_path: impl AsRef<Path>) -> Result<Self> {
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

    pub fn add_pem_from_file<CP>(mut self, cert_path: CP) -> Result<Self>
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
    pub fn build(self) -> Result<TlsConnector> {
        Self::build_rustls(self)
    }

    #[cfg(not(feature = "rustls"))]
    pub fn build(self) -> Result<TlsConnector> {
        Self::build_native_tls(self)
    }

    #[cfg(feature = "rustls")]
    pub fn build_rustls(self) -> Result<TlsConnector> {
        Ok(TlsConnector::Rustls(RustlsConnector::build(self)?))
    }

    #[cfg(feature = "native-tls")]
    pub fn build_native_tls(self) -> Result<TlsConnector> {
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
    pub fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> Result<Self> {
        Ok(Self {
            acceptor: TlsAcceptor::from_pem(cert, key)?,
        })
    }

    pub fn from_pem_file<CP, KP>(cert_path: CP, key_path: KP) -> Result<Self>
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
                let tcp = make_tcp_connection(&self.cfg, addr).await?;

                match &self.tls_config.connector {
                    #[cfg(feature = "rustls")]
                    TlsConnector::Rustls(connector) => {
                        connector.connect(&self.tls_config.server_name, tcp).await
                    }
                    #[cfg(feature = "native-tls")]
                    TlsConnector::NativeTls(connector) => {
                        connector.connect(&self.tls_config.server_name, tcp).await
                    }
                }
            }
            #[cfg(target_family = "unix")]
            Address::Unix(addr) => UnixStream::connect(addr.as_pathname().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "cannot connect to unnamed socket",
                )
            })?)
            .await
            .map(Conn::from),
        }
    }
}

impl MakeTransport for TlsMakeTransport {
    type ReadHalf = OwnedReadHalf;
    type WriteHalf = OwnedWriteHalf;

    async fn make_transport(&self, addr: Address) -> Result<(Self::ReadHalf, Self::WriteHalf)> {
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
