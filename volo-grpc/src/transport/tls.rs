#[derive(Clone)]
pub struct ServerTlsConfig {
    pub acceptor: TlsAcceptor,
}

#[derive(Clone)]
pub enum TlsAcceptor {
    #[cfg(feature = "rustls")]
    Rustls(tokio_rustls::TlsAcceptor),
    
    #[cfg(feature = "native-tls")]
    NativeTls(tokio_native_tls::TlsAcceptor),
}

impl From<tokio_rustls::TlsAcceptor> for ServerTlsConfig {
    fn from(value: tokio_rustls::TlsAcceptor) -> Self {
        Self {
            acceptor: TlsAcceptor::Rustls(value),
        }
    }
}

impl From<tokio_native_tls::TlsAcceptor> for ServerTlsConfig {
    fn from(value: tokio_native_tls::TlsAcceptor) -> Self {
        Self {
            acceptor: TlsAcceptor::NativeTls(value),
        }
    }
}