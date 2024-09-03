use std::{io, marker::PhantomData};

use motore::service::{Service, UnaryService};
use pilota::thrift::TransportException;
use volo::{
    net::{conn::ConnExt, dial::MakeTransport, shm::TransportEndpoint, Address},
    FastStr,
};

use crate::{
    codec::MakeCodec,
    context::ClientContext,
    protocol::TMessageType,
    transport::{
        pingpong::thrift_transport::ThriftTransport,
        pool::{Config, PooledMakeTransport, Transport, Ver},
    },
    EntryMessage, ThriftMessage,
};

#[derive(Clone)]
pub struct MakeClientTransport<MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf>,
{
    make_transport: MkT,
    make_codec: MkC,
}

impl<MkT, MkC> MakeClientTransport<MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf>,
{
    #[allow(unused)]
    #[inline]
    pub fn new(make_transport: MkT, make_codec: MkC) -> Self {
        Self {
            make_transport,
            make_codec,
        }
    }
}

impl<MkT, MkC> UnaryService<Address> for MakeClientTransport<MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
{
    type Response = ThriftTransport<MkC::Encoder, MkC::Decoder>;
    type Error = io::Error;

    #[inline]
    async fn call(&self, target: Address) -> Result<Self::Response, Self::Error> {
        let make_transport = self.make_transport.clone();
        let conn = make_transport.make_transport(target).await?;
        let inner = conn.inner();
        let (rh, wh) = conn.into_split();
        Ok(ThriftTransport::new(rh, wh, self.make_codec.clone(), inner))
    }
}

pub struct Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
{
    #[allow(clippy::type_complexity)]
    make_transport: PooledMakeTransport<MakeClientTransport<MkT, MkC>, Address>,
    _marker: PhantomData<Resp>,
}

impl<Resp, MkT, MkC> Clone for Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
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
    Resp: EntryMessage + Sync,
    MkT: MakeTransport,
    MkC: MakeCodec<<MkT::Conn as ConnExt>::ReadHalf, <MkT::Conn as ConnExt>::WriteHalf> + Sync,
{
    type Response = Option<ThriftMessage<Resp>>;

    type Error = crate::ClientError;

    #[inline]
    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ClientContext,
        req: ThriftMessage<Req>,
    ) -> Result<Self::Response, Self::Error> {
        let rpc_info = &cx.rpc_info;
        let target = rpc_info.callee().address().ok_or_else(|| {
            TransportException::from(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("address is required, rpc_info: {:?}", rpc_info),
            ))
        })?;
        let shmipc_target = rpc_info.callee().shmipc_address();
        let oneway = cx.message_type == TMessageType::OneWay;
        cx.stats.record_make_transport_start_at();
        let mut transport = self
            .make_transport
            .call((target, shmipc_target.clone(), Ver::PingPong))
            .await?;
        cx.stats.record_make_transport_end_at();
        if let Transport::Shm(_) = transport {
            cx.rpc_info
                .caller_mut()
                .set_transport(volo::net::shm::Transport(FastStr::new("shmipc")))
        }
        let resp = transport.send(cx, req, oneway).await;
        if let Ok(None) = resp {
            if !oneway {
                return Err(crate::ClientError::Transport(
                    pilota::thrift::TransportException::from(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "an unexpected end of file from server, rpc_info: {:?}",
                            cx.rpc_info
                        ),
                    )),
                ));
            }
        }
        if cx.transport.should_reuse && resp.is_ok() {
            transport.reuse().await;
        } else {
            transport.close().await;
        }
        resp
    }
}
