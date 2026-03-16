use std::io;

use super::{
    Address, DefaultIncoming, MakeIncoming,
    conn::{Conn, OwnedReadHalf, OwnedWriteHalf},
    dial::{DefaultMakeTransport, MakeTransport},
    incoming::Incoming,
};

pub struct ShmipcAddressWithFallback<MI> {
    pub shmipc_addr: Address,
    pub default_mi: MI,
}

impl<MI, I> MakeIncoming for ShmipcAddressWithFallback<MI>
where
    MI: MakeIncoming<Incoming = I> + Send,
    I: Incoming + Send,
{
    type Incoming = ShmipcIncoming<I>;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        Ok(ShmipcIncoming {
            shmipc_listener: self.shmipc_addr.make_incoming().await?,
            default_incoming: self.default_mi.make_incoming().await?,
        })
    }
}

#[derive(Debug)]
pub struct ShmipcIncoming<I> {
    shmipc_listener: DefaultIncoming,
    default_incoming: I,
}

impl<I> Incoming for ShmipcIncoming<I>
where
    I: Incoming,
{
    async fn accept(&mut self) -> io::Result<Option<Conn>> {
        self.try_next().await
    }
}

impl<I> ShmipcIncoming<I>
where
    I: Incoming,
{
    async fn try_next(&mut self) -> io::Result<Option<Conn>> {
        tokio::select! {
            biased;
            conn = self.shmipc_listener.accept() => {
                tracing::trace!("recv a conn from shmipc");
                conn
            }
            conn = self.default_incoming.accept() => {
                tracing::trace!("recv a conn from default");
                conn
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShmipcMakeTransportWithFallback {
    pub shmipc_mkt: DefaultMakeTransport,
    pub default_mkt: DefaultMakeTransport,
    pub fallback_addr: Address,
}

impl ShmipcMakeTransportWithFallback {
    pub fn new(
        shmipc: DefaultMakeTransport,
        default_mkt: DefaultMakeTransport,
        fallback_addr: Address,
    ) -> Self {
        Self {
            shmipc_mkt: shmipc,
            default_mkt,
            fallback_addr,
        }
    }
}

impl MakeTransport for ShmipcMakeTransportWithFallback {
    type ReadHalf = OwnedReadHalf;
    type WriteHalf = OwnedWriteHalf;

    async fn make_transport(
        &self,
        mut addr: Address,
    ) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
        if addr.is_shmipc() {
            match self.shmipc_mkt.make_transport(addr).await {
                Ok(ret) => return Ok(ret),
                Err(e) => {
                    tracing::info!(
                        "failed to connect to shmipc target: {e}, fallback to default target"
                    );
                    addr = self.fallback_addr.clone();
                }
            }
        }

        self.default_mkt.make_transport(addr).await
    }

    fn set_connect_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.default_mkt.set_connect_timeout(timeout);
        self.shmipc_mkt.set_connect_timeout(timeout);
    }

    fn set_read_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.default_mkt.set_read_timeout(timeout);
        self.shmipc_mkt.set_read_timeout(timeout);
    }

    fn set_write_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.default_mkt.set_write_timeout(timeout);
        self.shmipc_mkt.set_write_timeout(timeout);
    }
}
