use std::{io, marker::PhantomData};

use motore::service::{Service, UnaryService};
use volo::net::{conn::ConnExt, dial::MakeTransport, Address};

use crate::{
    codec::MakeCodec,
    context::ClientContext,
    protocol::TMessageType,
    transport::{
        multiplex::thrift_transport::ThriftTransport,
        pool::{Config, PooledMakeTransport, Transport, Ver},
    },
    ClientError, EntryMessage, ThriftMessage,
};

pub struct MakeClientTransport<MkT, MkC, Resp>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf>,
{
    make_transport: MkT,
    make_codec: MkC,
    _phantom: PhantomData<fn() -> Resp>,
}

impl<
        MkT: MakeTransport,
        MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf>,
        Resp,
    > Clone for MakeClientTransport<MkT, MkC, Resp>
{
    fn clone(&self) -> Self {
        Self {
            make_transport: self.make_transport.clone(),
            make_codec: self.make_codec.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<MkT, MkC, Resp> MakeClientTransport<MkT, MkC, Resp>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf>,
{
    #[allow(unused)]
    pub fn new(make_transport: MkT, make_codec: MkC) -> Self {
        Self {
            make_transport,
            make_codec,
            _phantom: PhantomData,
        }
    }
}

impl<MkT, MkC, Resp> UnaryService<Address> for MakeClientTransport<MkT, MkC, Resp>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
    Resp: EntryMessage + Send + 'static,
{
    type Response = ThriftTransport<MkC::Encoder, Resp>;
    type Error = io::Error;

    async fn call(&self, target: Address) -> Result<Self::Response, Self::Error> {
        let make_transport = self.make_transport.clone();
        let conn = make_transport.make_transport(target.clone()).await?;
        let (rh, wh) = conn.into_split();
        Ok(ThriftTransport::new(
            rh,
            wh,
            self.make_codec.clone(),
            target,
        ))
    }
}

pub struct Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
    Resp: EntryMessage + Send + 'static,
{
    #[allow(clippy::type_complexity)]
    make_transport: PooledMakeTransport<MakeClientTransport<MkT, MkC, Resp>, Address>,
    _marker: PhantomData<Resp>,
}

impl<Resp, MkT, MkC> Clone for Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
    Resp: EntryMessage + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            make_transport: self.make_transport.clone(),
            _marker: self._marker,
        }
    }
}

impl<Resp, MkT, MkC> Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
    Resp: EntryMessage + Send + 'static,
{
    pub fn new(make_transport: MkT, pool_cfg: Option<Config>, make_codec: MkC) -> Self {
        let make_transport = MakeClientTransport::new(make_transport, make_codec);
        let make_transport = PooledMakeTransport::new(make_transport, pool_cfg);
        Client {
            make_transport,
            _marker: PhantomData,
        }
    }
}

impl<Req, Resp, MkT, MkC> Service<ClientContext, ThriftMessage<Req>> for Client<Resp, MkT, MkC>
where
    Req: Send + 'static + EntryMessage,
    Resp: EntryMessage + Send + 'static + Sync,
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
{
    type Response = Option<ThriftMessage<Resp>>;

    type Error = ClientError;

    async fn call<'cx, 's>(
        &'s self,
        cx: &'cx mut ClientContext,
        req: ThriftMessage<Req>,
    ) -> Result<Self::Response, Self::Error> {
        let rpc_info = &cx.rpc_info;
        let target = rpc_info.callee().address().ok_or_else(|| {
            let msg = format!("address is required, rpcinfo: {:?}", rpc_info);
            ClientError::Transport(io::Error::new(io::ErrorKind::InvalidData, msg).into())
        })?;
        let oneway = cx.message_type == TMessageType::OneWay;
        cx.stats.record_make_transport_start_at();
        let transport = self
            .make_transport
            .call((target, None, Ver::Multiplex))
            .await?;
        cx.stats.record_make_transport_end_at();
        let resp = transport.send(cx, req, oneway).await;
        if let Ok(None) = resp {
            if !oneway {
                return Err(ClientError::Transport(
                    pilota::thrift::TransportException::from(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("an unexpected end of file from server, cx: {:?}", cx),
                    )),
                ));
            }
        }
        if cx.transport.should_reuse && resp.is_ok() {
            if let Transport::Pooled(pooled) = transport {
                pooled.reuse().await;
            }
        }
        resp
    }
}
