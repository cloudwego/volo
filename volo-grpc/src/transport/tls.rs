#[derive(Clone)]
pub enum TlsAcceptorConfig {
    None,

    #[cfg(feature = "rustls")]
    Rustls(tokio_rustls::TlsAcceptor),
    
    #[cfg(feature = "native-tls")]
    NativeTls(tokio_native_tls::TlsAcceptor),
}

impl From<()> for TlsAcceptorConfig {
    fn from(value: ()) -> Self {
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