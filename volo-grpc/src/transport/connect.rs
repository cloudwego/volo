use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::BoxFuture;
use hyper::client::connect::{Connected, Connection};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use volo::net::{
    conn::Conn,
    dial::{Config, DefaultMakeTransport, MakeTransport},
    Address,
};

cfg_rustls_or_native_tls! {
    use volo::net::dial::{TlsMakeTransport, ClientTlsConfig};
}

#[derive(Clone, Debug)]
pub enum Connector {
    Default(DefaultMakeTransport),
    #[cfg(any(feature = "rustls", feature = "native-tls"))]
    Tls(TlsMakeTransport),
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

    #[cfg(any(feature = "rustls", feature = "native-tls"))]
    pub fn new_with_tls(cfg: Option<Config>, tls_config: ClientTlsConfig) -> Self {
        let mut mt = TlsMakeTransport::new(cfg.unwrap_or_default(), tls_config);
        if let Some(cfg) = cfg {
            mt.set_connect_timeout(cfg.connect_timeout);
            mt.set_read_timeout(cfg.read_timeout);
            mt.set_write_timeout(cfg.write_timeout);
        }
        Self::Tls(mt)
    }
}

impl Default for Connector {
    fn default() -> Self {
        Self::new(None)
    }
}

impl tower::Service<hyper::Uri> for Connector {
    type Response = ConnectionWrapper;

    type Error = io::Error;

    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: hyper::Uri) -> Self::Future {
        macro_rules! box_pin_call {
            ($mk_conn:ident) => {
                Box::pin(async move {
                    let authority = uri.authority().expect("authority required").as_str();
                    let target: Address = match uri.scheme_str() {
                        Some("http") => {
                            Address::Ip(authority.parse::<SocketAddr>().map_err(|_| {
                                io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    "authority must be valid SocketAddr",
                                )
                            })?)
                        }
                        #[cfg(target_family = "unix")]
                        Some("http+unix") => {
                            use hex::FromHex;

                            let bytes = Vec::from_hex(authority).map_err(|_| {
                                io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    "authority must be hex-encoded path",
                                )
                            })?;
                            Address::Unix(std::borrow::Cow::Owned(
                                String::from_utf8(bytes)
                                    .map_err(|_| {
                                        io::Error::new(
                                            io::ErrorKind::InvalidInput,
                                            "authority must be valid UTF-8",
                                        )
                                    })?
                                    .into(),
                            ))
                        }
                        _ => unimplemented!(),
                    };

                    Ok(ConnectionWrapper($mk_conn.make_connection(target).await?))
                })
            };
        }

        match self {
            Self::Default(mk_conn) => {
                let mk_conn = mk_conn.clone();
                box_pin_call!(mk_conn)
            }
            #[cfg(any(feature = "rustls", feature = "native-tls"))]
            Self::Tls(mk_conn) => {
                let mk_conn = mk_conn.clone();
                box_pin_call!(mk_conn)
            }
        }
    }
}

pub struct ConnectionWrapper(Conn);

impl AsyncRead for ConnectionWrapper {
    #[inline]
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for ConnectionWrapper {
    #[inline]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    #[inline]
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    #[inline]
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl Connection for ConnectionWrapper {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[cfg(test)]
mod tests {
    use hex::FromHex;

    #[test]
    fn test_convert() {
        let authority = "2f746d702f7270632e736f636b";
        assert_eq!(
            String::from_utf8(Vec::from_hex(authority).unwrap()).unwrap(),
            "/tmp/rpc.sock"
        );
    }
}
