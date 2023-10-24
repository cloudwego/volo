pub struct ServerTlsConfig {
    acceptor: Option<TlsAcceptor>,
}

#[derive(Clone)]
pub enum TlsAcceptor {
    #[cfg(feature = "rustls")]
    Rustls(tokio_rustls::TlsAcceptor),
    
    #[cfg(feature = "native-tls")]
    NativeTls(tokio_native_tls::TlsAcceptor),
}

impl From<()> for TlsAcceptorConfig {
    fn from(_value: ()) -> Self {
        Self::None
    }
}

impl From<tokio_rustls::TlsAcceptor> for TlsAcceptorConfig {
    fn from(value: tokio_rustls::TlsAcceptor) -> Self {
        Self::Rustls(value)
    }
}

impl From<tokio_native_tls::TlsAcceptor> for TlsAcceptorConfig {
    fn from(value: tokio_native_tls::TlsAcceptor) -> Self {
        Self::NativeTls(value)
    }
}