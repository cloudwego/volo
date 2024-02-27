use std::{io, time::Duration};

use motore::{make::MakeConnection, UnaryService};
use tokio::net::UnixStream;

use super::{make_tcp_connection, Config, MakeTransport};
use crate::net::{
    conn::{Conn, OwnedReadHalf, OwnedWriteHalf},
    Address,
};

/// A wrapper around [`tokio_rustls::TlsConnector`] and [`tokio_native_tls::TlsConnector`].
#[derive(Clone)]
pub enum TlsConnector {
    #[cfg(feature = "rustls")]
    Rustls(tokio_rustls::TlsConnector),

    /// This takes an `Arc` because `tokio_native_tls::TlsConnector` does not internally use `Arc`
    #[cfg(feature = "native-tls")]
    NativeTls(std::sync::Arc<tokio_native_tls::TlsConnector>),
}

impl std::fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "rustls")]
            Self::Rustls(_) => f.debug_tuple("Rustls").finish(),

            #[cfg(feature = "native-tls")]
            Self::NativeTls(_) => f.debug_tuple("NativeTls").finish(),
        }
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

    async fn call(&self, addr: Address) -> Result<Self::Response, Self::Error> {
        match addr {
            Address::Ip(addr) => {
                let tcp = make_tcp_connection(&self.cfg, addr).await?;

                match &self.tls_config.connector {
                    #[cfg(feature = "rustls")]
                    TlsConnector::Rustls(connector) => {
                        let server_name = rustls::pki_types::ServerName::try_from(
                            &self.tls_config.server_name[..],
                        )
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
                        .to_owned();
                        connector
                            .connect(server_name, tcp)
                            .await
                            .map(tokio_rustls::TlsStream::Client)
                            .map(Conn::from)
                    }
                    #[cfg(feature = "native-tls")]
                    TlsConnector::NativeTls(connector) => connector
                        .connect(&self.tls_config.server_name[..], tcp)
                        .await
                        .map(Conn::from)
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, e)),
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

#[cfg(feature = "rustls")]
impl From<tokio_rustls::TlsConnector> for TlsConnector {
    fn from(value: tokio_rustls::TlsConnector) -> Self {
        Self::Rustls(value)
    }
}

#[cfg(feature = "native-tls")]
impl From<std::sync::Arc<tokio_native_tls::TlsConnector>> for TlsConnector {
    fn from(value: std::sync::Arc<tokio_native_tls::TlsConnector>) -> Self {
        Self::NativeTls(value)
    }
}

#[cfg(feature = "native-tls")]
impl From<tokio_native_tls::TlsConnector> for TlsConnector {
    fn from(value: tokio_native_tls::TlsConnector) -> Self {
        Self::NativeTls(std::sync::Arc::new(value))
    }
}
