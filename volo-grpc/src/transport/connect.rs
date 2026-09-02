use std::io;

use http::uri::Scheme;
use motore::{make::MakeConnection, service::UnaryService};
#[cfg(feature = "__tls")]
#[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
use volo::net::tls::{ClientTlsConfig, TlsMakeTransport};
use volo::net::{
    Address,
    conn::Conn,
    dial::{Config, DefaultMakeTransport, MakeTransport},
};

/// Dials the callee address picked by service discovery and load balancing.
///
/// The connector only knows about [`Address`]es; the request URI (and therefore the HTTP/2
/// `:authority`) is chosen independently by the transport, see
/// [`ClientTransport`][super::ClientTransport].
#[derive(Clone, Debug)]
pub enum Connector {
    Default(DefaultMakeTransport),
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    Tls {
        transport: TlsMakeTransport,
        /// The SNI server name, kept so the transport can reuse it as `:authority`.
        server_name: volo::FastStr,
    },
}

impl Connector {
    pub fn new(cfg: Option<Config>) -> Self {
        let mut mt = DefaultMakeTransport::default();
        if let Some(cfg) = cfg {
            mt.set_connect_timeout(cfg.connect_timeout);
            mt.set_read_timeout(cfg.read_timeout);
            mt.set_write_timeout(cfg.write_timeout);
        }
        Self::Default(mt)
    }

    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn new_with_tls(cfg: Option<Config>, tls_config: ClientTlsConfig) -> Self {
        let server_name = volo::FastStr::new(&tls_config.server_name);
        let mut mt = TlsMakeTransport::new(cfg.unwrap_or_default(), tls_config);
        if let Some(cfg) = cfg {
            mt.set_connect_timeout(cfg.connect_timeout);
            mt.set_read_timeout(cfg.read_timeout);
            mt.set_write_timeout(cfg.write_timeout);
        }
        Self::Tls {
            transport: mt,
            server_name,
        }
    }

    /// The URI scheme matching the transport security, sent as the `:scheme` pseudo-header.
    pub fn scheme(&self) -> Scheme {
        match self {
            Self::Default(_) => Scheme::HTTP,
            #[cfg(feature = "__tls")]
            Self::Tls { .. } => Scheme::HTTPS,
        }
    }

    /// The server name used for SNI, if the connector speaks TLS.
    pub fn tls_server_name(&self) -> Option<&str> {
        match self {
            Self::Default(_) => None,
            #[cfg(feature = "__tls")]
            Self::Tls { server_name, .. } => Some(server_name),
        }
    }
}

impl Default for Connector {
    fn default() -> Self {
        Self::new(None)
    }
}

impl UnaryService<Address> for Connector {
    type Response = Conn;
    type Error = io::Error;

    async fn call(&self, addr: Address) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Default(mkt) => mkt.make_connection(addr).await,
            #[cfg(feature = "__tls")]
            Self::Tls { transport, .. } => transport.make_connection(addr).await,
        }
    }
}
