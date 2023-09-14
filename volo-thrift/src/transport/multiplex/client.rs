use std::{io, marker::PhantomData};

use futures::Future;
use motore::service::{Service, UnaryService};
use pilota::thrift::TransportErrorKind;
use volo::{
    net::{dial::MakeTransport, Address},
    Unwrap,
};

use crate::{
    codec::MakeCodec,
    context::ClientContext,
    protocol::TMessageType,
    transport::{
        multiplex::thrift_transport::ThriftTransport,
        pool::{Config, PooledMakeTransport},
    },
    EntryMessage, Error, ThriftMessage,
};

pub struct MakeClientTransport<MkT, MkC, Resp>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf>,
{
    make_transport: MkT,
    make_codec: MkC,
    _phantom: PhantomData<fn() -> Resp>,
}

impl<MkT: MakeTransport, MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf>, Resp> Clone
    for MakeClientTransport<MkT, MkC, Resp>
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
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf>,
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
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
    Resp: EntryMessage + Send + 'static,
{
    type Response = ThriftTransport<MkC::Encoder, Resp>;
    type Error = io::Error;
    type Future<'s> = impl Future<Output = Result<Self::Response, Self::Error>> + 's;

    fn call(&self, target: Address) -> Self::Future<'_> {
        let make_transport = self.make_transport.clone();
        async move {
            let (rh, wh) = make_transport.make_transport(target.clone()).await?;
            Ok(ThriftTransport::new(
                rh,
                wh,
                self.make_codec.clone(),
                target,
            ))
        }
    }
}

pub struct Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
    Resp: EntryMessage + Send + 'static,
{
    #[allow(clippy::type_complexity)]
    make_transport: PooledMakeTransport<MakeClientTransport<MkT, MkC, Resp>, Address>,
    _marker: PhantomData<Resp>,
}

impl<Resp, MkT, MkC> Clone for Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
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
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
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
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    type Response = Option<ThriftMessage<Resp>>;

    type Error = crate::Error;

    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'cx where Self:'cx;

    fn call<'cx, 's>(
        &'s self,
        cx: &'cx mut ClientContext,
        req: ThriftMessage<Req>,
    ) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move {
            let rpc_info = &cx.rpc_info;
            let target = rpc_info.callee().volo_unwrap().address().ok_or_else(|| {
                let msg = format!("address is required, rpcinfo: {:?}", rpc_info);
                crate::Error::Transport(io::Error::new(io::ErrorKind::InvalidData, msg).into())
            })?;
            let oneway = cx.message_type == TMessageType::OneWay;
            cx.stats.record_make_transport_start_at();
            let transport = self.make_transport.call(target).await?;
            cx.stats.record_make_transport_end_at();
            let resp = transport.send(cx, req, oneway).await;
            if let Ok(None) = resp {
                if !oneway {
                    return Err(Error::Transport(pilota::thrift::TransportError::new(
                        TransportErrorKind::EndOfFile,
                        format!(
                            "an unexpected end of file from server, cx: {:?}",
                            cx
                        ),
                    )));
                }
            }
            if cx.transport.should_reuse && resp.is_ok() {
                transport.reuse();
            }
            resp
        }
    }
}
