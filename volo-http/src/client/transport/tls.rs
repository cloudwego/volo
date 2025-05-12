use http::uri::Scheme;
use motore::service::Service;
use volo::{
    context::Context,
    net::{
        conn::{Conn, ConnStream},
        tls::{Connector, TlsConnector},
        Address,
    },
};

use super::plain::PlainMakeConnection;
use crate::{
    context::ClientContext,
    error::{client::request_error, ClientError},
};

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

impl<S> Service<ClientContext, Address> for TlsMakeConnection<S>
where
    S: Service<ClientContext, Address, Response = Conn, Error = ClientError> + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: Address,
    ) -> Result<Self::Response, Self::Error> {
        let conn = self.inner.call(cx, req).await?;

        if cx.scheme() == &Scheme::HTTP {
            // It's an HTTP request
            return Ok(conn);
        }

        let target_name = cx.rpc_info().callee().service_name_ref();
        tracing::debug!("[Volo-HTTP] try to make tls handshake, name: {target_name:?}");

        let tcp_stream = match conn.stream {
            ConnStream::Tcp(tcp_stream) => tcp_stream,
            _ => unreachable!(),
        };
        match self.tls_connector.connect(target_name, tcp_stream).await {
            Ok(conn) => Ok(conn),
            Err(err) => {
                tracing::error!("[Volo-HTTP] failed to make tls connection, error: {err}");
                Err(request_error(err))
            }
        }
    }
}
