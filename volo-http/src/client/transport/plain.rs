use std::error::Error;

use motore::{make::MakeConnection, service::UnaryService};
use volo::net::{Address, dial::DefaultMakeTransport};

use super::connector::PeerInfo;
use crate::error::{ClientError, client::request_error};

#[derive(Clone, Debug)]
pub struct PlainMakeConnection<MkC = DefaultMakeTransport> {
    mk_conn: MkC,
}

impl<MkC> PlainMakeConnection<MkC>
where
    MkC: MakeConnection<Address>,
{
    pub fn new(mk_conn: MkC) -> Self {
        Self { mk_conn }
    }
}

impl Default for PlainMakeConnection<DefaultMakeTransport> {
    fn default() -> Self {
        Self::new(DefaultMakeTransport::new())
    }
}

impl<MkC> UnaryService<PeerInfo> for PlainMakeConnection<MkC>
where
    MkC: MakeConnection<Address> + Sync,
    MkC::Error: Error + Send + Sync + 'static,
{
    type Response = MkC::Connection;
    type Error = ClientError;

    async fn call(&self, req: PeerInfo) -> Result<Self::Response, Self::Error> {
        tracing::debug!("[Volo-HTTP] connecting to target: {req:?}");
        match self.mk_conn.make_connection(req.address.clone()).await {
            Ok(conn) => Ok(conn),
            Err(err) => {
                tracing::error!("[Volo-HTTP] failed to make connection, error: {err}");
                Err(request_error(err).with_address(req.address))
            }
        }
    }
}
