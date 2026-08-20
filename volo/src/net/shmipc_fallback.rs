use std::io;

use super::{Address, DefaultIncoming, MakeIncoming, conn::Conn, incoming::Incoming};

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
