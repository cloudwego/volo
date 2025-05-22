use http::uri::Scheme;
use motore::service::UnaryService;
use volo::net::{
    conn::{Conn, ConnStream},
    tls::{Connector, TlsConnector},
};

use super::{connector::PeerInfo, plain::PlainMakeConnection};
use crate::error::{client::request_error, ClientError};

#[derive(Clone, Debug)]
pub struct TlsMakeConnection<S = PlainMakeConnection> {
    inner: S,
    tls_connector: TlsConnector,
}

impl<S> TlsMakeConnection<S> {
    pub fn new(inner: S, tls_connector: TlsConnector) -> Self {
        Self {
            inner,
            tls_connector,
        }
    }
}

impl<S> UnaryService<PeerInfo> for TlsMakeConnection<S>
where
    S: UnaryService<PeerInfo, Response = Conn, Error = ClientError> + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(&self, req: PeerInfo) -> Result<Self::Response, Self::Error> {
        let conn = self.inner.call(req.clone()).await?;

        if req.scheme == Scheme::HTTP {
            // It's an HTTP request
            return Ok(conn);
        }

        let target_name = req.name;
        tracing::debug!("[Volo-HTTP] try to make tls handshake, name: {target_name:?}");

        let tcp_stream = match conn.stream {
            ConnStream::Tcp(tcp_stream) => tcp_stream,
            _ => unreachable!(),
        };
        match self.tls_connector.connect(&target_name, tcp_stream).await {
            Ok(conn) => Ok(conn),
            Err(err) => {
                tracing::error!("[Volo-HTTP] failed to make tls connection, error: {err}");
                Err(request_error(err))
            }
        }
    }
}
