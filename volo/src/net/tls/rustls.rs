use std::{io, io::Result, path::Path, sync::Arc};

use rustls::{pki_types::ServerName, RootCertStore, ServerConfig};
use rustls_pki_types::PrivateKeyDer;
use tokio::net::TcpStream;
use tokio_rustls::{rustls::ClientConfig, TlsAcceptor, TlsConnector};

use super::{Acceptor, Connector, TlsConnectorBuilder};
use crate::net::conn::{Conn, ConnStream};

/// A wrapper for [`tokio_rustls::TlsConnector`]
#[derive(Clone)]
pub struct RustlsConnector(pub(super) TlsConnector);

/// A wrapper for [`tokio_rustls::TlsAcceptor`]
#[derive(Clone)]
pub struct RustlsAcceptor(pub(super) TlsAcceptor);

impl Default for RustlsConnector {
    fn default() -> Self {
        Self::build(TlsConnectorBuilder::default()).expect("Failed to create RustlsConnector")
    }
}

impl Connector for RustlsConnector {
    fn build(builder: TlsConnectorBuilder) -> Result<Self> {
        let mut certs = if builder.default_root_certs {
            RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_owned(),
            }
        } else {
            RootCertStore::empty()
        };
        for pem in builder.pems.into_iter() {
            let cert = rustls_pemfile::certs(&mut pem.as_ref())
                .next()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "No certificate found in the provided PEM",
                    )
                })??;
            certs
                .add(cert)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        }
        let mut client_config = ClientConfig::builder()
            .with_root_certificates(certs)
            .with_no_client_auth();
        client_config.alpn_protocols = builder.alpn_protocols;
        let connector = TlsConnector::from(Arc::new(client_config));
        Ok(Self(connector))
    }

    async fn connect(&self, server_name: &str, tcp_stream: TcpStream) -> Result<Conn> {
        let sni = ServerName::try_from(server_name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
            .to_owned();
        tracing::trace!("RustlsConnector::connect({server_name:?})");
        self.0
            .connect(sni, tcp_stream)
            .await
            .map(tokio_rustls::TlsStream::Client)
            .map(Conn::from)
    }
}

impl Acceptor for RustlsAcceptor {
    fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> Result<Self> {
        let cert = rustls_pemfile::certs(&mut cert.as_ref()).collect::<Result<Vec<_>>>()?;
        let key = rustls_pemfile::pkcs8_private_keys(&mut key.as_ref())
            .collect::<Result<Vec<_>>>()?
            .pop()
            .map(PrivateKeyDer::Pkcs8)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "No private key found"))?;
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert, key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        Ok(Self(acceptor))
    }

    fn from_pem_file(cert_path: impl AsRef<Path>, key_path: impl AsRef<Path>) -> Result<Self> {
        let cert = std::fs::read(cert_path.as_ref())?;
        let key = std::fs::read(key_path.as_ref())?;
        Self::from_pem(cert, key)
    }

    async fn accept(&self, tcp_stream: TcpStream) -> Result<ConnStream> {
        tracing::trace!("RustlsAcceptor::accept");
        self.0
            .accept(tcp_stream)
            .await
            .map(tokio_rustls::TlsStream::Server)
            .map(ConnStream::from)
    }
}

impl From<ClientConfig> for super::TlsConnector {
    fn from(client_config: ClientConfig) -> Self {
        Self::Rustls(RustlsConnector(TlsConnector::from(Arc::new(client_config))))
    }
}

impl From<Arc<ClientConfig>> for super::TlsConnector {
    fn from(client_config: Arc<ClientConfig>) -> Self {
        Self::Rustls(RustlsConnector(TlsConnector::from(client_config)))
    }
}

impl From<TlsConnector> for super::TlsConnector {
    fn from(connector: TlsConnector) -> Self {
        Self::Rustls(RustlsConnector(connector))
    }
}

impl From<ServerConfig> for super::TlsAcceptor {
    fn from(server_config: ServerConfig) -> Self {
        Self::Rustls(RustlsAcceptor(TlsAcceptor::from(Arc::new(server_config))))
    }
}

impl From<Arc<ServerConfig>> for super::TlsAcceptor {
    fn from(server_config: Arc<ServerConfig>) -> Self {
        Self::Rustls(RustlsAcceptor(TlsAcceptor::from(server_config)))
    }
}

impl From<TlsAcceptor> for super::TlsAcceptor {
    fn from(acceptor: TlsAcceptor) -> Self {
        Self::Rustls(RustlsAcceptor(acceptor))
    }
}
