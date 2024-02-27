/// TLS configuration for a server.
#[derive(Clone)]
pub struct ServerTlsConfig {
    pub acceptor: TlsAcceptor,
}

/// A wrapper around [`tokio_rustls::TlsAcceptor`] and [`tokio_native_tls::TlsAcceptor`].
#[derive(Clone)]
pub enum TlsAcceptor {
    /// `tokio_rustls::TlsAcceptor` internally uses `Arc`
    #[cfg_attr(docsrs, doc(cfg(feature = "rustls")))]
    #[cfg(feature = "rustls")]
    Rustls(tokio_rustls::TlsAcceptor),

    /// This takes an `Arc` because it does not internally use `Arc`
    #[cfg_attr(docsrs, doc(cfg(feature = "native-tls")))]
    #[cfg(feature = "native-tls")]
    NativeTls(std::sync::Arc<tokio_native_tls::TlsAcceptor>),
}

#[cfg(feature = "rustls")]
impl From<tokio_rustls::TlsAcceptor> for ServerTlsConfig {
    fn from(value: tokio_rustls::TlsAcceptor) -> Self {
        Self {
            acceptor: TlsAcceptor::Rustls(value),
        }
    }
}

#[cfg(feature = "native-tls")]
impl From<std::sync::Arc<tokio_native_tls::TlsAcceptor>> for ServerTlsConfig {
    fn from(value: std::sync::Arc<tokio_native_tls::TlsAcceptor>) -> Self {
        Self {
            acceptor: TlsAcceptor::NativeTls(value),
        }
    }
}

#[cfg(feature = "native-tls")]
impl From<tokio_native_tls::TlsAcceptor> for ServerTlsConfig {
    fn from(value: tokio_native_tls::TlsAcceptor) -> Self {
        Self {
            acceptor: TlsAcceptor::NativeTls(std::sync::Arc::new(value)),
        }
    }
}
