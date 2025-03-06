use std::{io, io::Result, path::Path, sync::Arc};

use native_tls::{Certificate, Identity};
use tokio::net::TcpStream;
use tokio_native_tls::{TlsAcceptor, TlsConnector};

use super::{Acceptor, Conn, Connector, TlsConnectorBuilder};
use crate::net::conn::ConnStream;

/// A wrapper for [`tokio_native_tls::TlsConnector`]
#[derive(Clone)]
pub struct NativeTlsConnector(pub(super) Arc<TlsConnector>);

/// A wrapper for [`tokio_native_tls::TlsAcceptor`]
#[derive(Clone)]
pub struct NativeTlsAcceptor(pub(super) Arc<TlsAcceptor>);

impl Default for NativeTlsConnector {
    fn default() -> Self {
        Self::build(TlsConnectorBuilder::default()).expect("Failed to create NativeTlsConnector")
    }
}

impl Connector for NativeTlsConnector {
    fn build(config: TlsConnectorBuilder) -> Result<Self> {
        let mut builder = native_tls::TlsConnector::builder();
        builder.disable_built_in_roots(!config.default_root_certs);
        for pem in config.pems {
            let cert = Certificate::from_pem(pem.as_ref())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            builder.add_root_certificate(cert);
        }
        let connector = builder
            .build()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        Ok(Self(Arc::new(TlsConnector::from(connector))))
    }

    async fn connect(&self, server_name: &str, tcp_stream: TcpStream) -> Result<Conn> {
        tracing::trace!("NativeTlsConnector::connect({server_name})");
        self.0
            .connect(server_name, tcp_stream)
            .await
            .map(Conn::from)
            .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))
    }
}

impl Acceptor for NativeTlsAcceptor {
    fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> Result<Self> {
        let identity = Identity::from_pkcs8(&cert, &key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        Ok(Self(Arc::new(
            native_tls::TlsAcceptor::builder(identity)
                .build()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
                .into(),
        )))
    }

    fn from_pem_file(cert_path: impl AsRef<Path>, key_path: impl AsRef<Path>) -> Result<Self> {
        let cert = std::fs::read(cert_path.as_ref())?;
        let key = std::fs::read(key_path.as_ref())?;
        Self::from_pem(cert, key)
    }

    async fn accept(&self, tcp_stream: TcpStream) -> Result<ConnStream> {
        tracing::trace!("NativeTlsAcceptor::accept");
        self.0
            .accept(tcp_stream)
            .await
            .map(ConnStream::from)
            .map_err(io::Error::other)
    }
}

impl From<native_tls::TlsConnector> for super::TlsConnector {
    fn from(value: native_tls::TlsConnector) -> Self {
        Self::NativeTls(NativeTlsConnector(Arc::new(TlsConnector::from(value))))
    }
}

impl From<TlsConnector> for super::TlsConnector {
    fn from(value: TlsConnector) -> Self {
        Self::NativeTls(NativeTlsConnector(Arc::new(value)))
    }
}

impl From<Arc<TlsConnector>> for super::TlsConnector {
    fn from(value: Arc<TlsConnector>) -> Self {
        Self::NativeTls(NativeTlsConnector(value))
    }
}

impl From<native_tls::TlsAcceptor> for super::TlsAcceptor {
    fn from(value: native_tls::TlsAcceptor) -> Self {
        Self::NativeTls(NativeTlsAcceptor(Arc::new(TlsAcceptor::from(value))))
    }
}

impl From<TlsAcceptor> for super::TlsAcceptor {
    fn from(value: TlsAcceptor) -> Self {
        Self::NativeTls(NativeTlsAcceptor(Arc::new(value)))
    }
}

impl From<Arc<TlsAcceptor>> for super::TlsAcceptor {
    fn from(value: Arc<TlsAcceptor>) -> Self {
        Self::NativeTls(NativeTlsAcceptor(value))
    }
}
