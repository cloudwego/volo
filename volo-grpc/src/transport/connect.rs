use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

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
        let mut mt = TlsMakeTransport::new(cfg.clone().unwrap_or_default(), tls_config);
        if let Some(cfg) = cfg {
            mt.set_connect_timeout(cfg.connect_timeout);
            mt.set_read_timeout(cfg.read_timeout);
            mt.set_write_timeout(cfg.write_timeout);
        }
        Self::Tls(mt)
    }

    pub async fn connect(&self, addr: Address) -> Result<Conn, io::Error> {
        match self {
            Self::Default(mk_conn) => {
                let mk_conn = mk_conn.clone();
                mk_conn.make_connection(addr).await
            }
            #[cfg(any(feature = "rustls", feature = "native-tls"))]
            Self::Tls(mk_conn) => {
                let mk_conn = mk_conn.clone();
                mk_conn.make_connection(addr).await
            }
        }
    }
}

impl Default for Connector {
    fn default() -> Self {
        Self::new(None)
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
