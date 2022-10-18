use std::{io, marker::PhantomData};

use futures::Future;
use motore::service::{Service, UnaryService};
use pilota::thrift::{new_transport_error, TransportErrorKind};
use volo::{
    net::{dial::MakeConnection, Address},
    Unwrap,
};

use crate::{
    codec::{CodecType, MkDecoder, MkEncoder},
    context::ClientContext,
    protocol::TMessageType,
    transport::{
        multiplex::thrift_transport::ThriftTransport,
        pool::{Config, PooledMakeTransport},
    },
    EntryMessage, Error, ThriftMessage,
};

pub struct MakeTransport<MkE, MkD, Resp> {
    make_connection: MakeConnection,
    codec_type: CodecType,
    mk_encoder: MkE,
    mk_decoder: MkD,
    _phantom: PhantomData<fn() -> Resp>,
}

impl<MkE: Clone, MkD: Clone, Resp> Clone for MakeTransport<MkE, MkD, Resp> {
    fn clone(&self) -> Self {
        Self {
            make_connection: self.make_connection.clone(),
            codec_type: self.codec_type,
            mk_decoder: self.mk_decoder.clone(),
            mk_encoder: self.mk_encoder.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<E, D, Resp> MakeTransport<E, D, Resp> {
    #[allow(unused)]
    pub fn new(
        make_connection: MakeConnection,
        codec_type: CodecType,
        mk_encoder: E,
        mk_decoder: D,
    ) -> Self {
        Self {
            make_connection,
            codec_type,
            mk_decoder,
            mk_encoder,
            _phantom: PhantomData,
        }
    }
}

impl<E, D, Resp> UnaryService<Address> for MakeTransport<E, D, Resp>
where
    E: MkEncoder + Send + 'static,
    D: MkDecoder + Send + 'static,
    Resp: EntryMessage + Send + 'static,
{
    type Response = ThriftTransport<E::Target, Resp>;
    type Error = io::Error;
    type Future<'s> = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn call(&mut self, target: Address) -> Self::Future<'_> {
        let make_connection = self.make_connection.clone();
        async move {
            let conn = make_connection.make_connection(target).await?;
            let decoder = self.mk_decoder.mk_decoder(Some(self.codec_type));
            let encoder = self.mk_encoder.mk_encoder(Some(self.codec_type));
            Ok(ThriftTransport::new(conn, encoder, decoder))
        }
    }
}

pub struct Client<Resp, MkE, MkD>
where
    MkE: MkEncoder + Send + 'static,
    MkD: MkDecoder + Send + 'static,
    Resp: EntryMessage + Send + 'static,
{
    #[allow(clippy::type_complexity)]
    make_transport: PooledMakeTransport<MakeTransport<MkE, MkD, Resp>, Address>,
    _maker: PhantomData<Resp>,
}

impl<Resp, MkE, MkD> Clone for Client<Resp, MkE, MkD>
where
    MkE: MkEncoder + Send + 'static,
    MkD: MkDecoder + Send + 'static,
    Resp: EntryMessage + Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            make_transport: self.make_transport.clone(),
            _maker: self._maker,
        }
    }
}

impl<Resp, MkE, MkD> Client<Resp, MkE, MkD>
where
    MkE: MkEncoder + Send + 'static,
    MkD: MkDecoder + Send + 'static,
    Resp: EntryMessage + Send + 'static,
{
    pub fn new(
        make_connection: MakeConnection,
        codec_type: CodecType,
        pool_cfg: Option<Config>,
        mk_encoder: MkE,
        mk_decoder: MkD,
    ) -> Self {
        let make_transport =
            MakeTransport::new(make_connection, codec_type, mk_encoder, mk_decoder);
        let make_transport = PooledMakeTransport::new(make_transport, pool_cfg);
        Client {
            make_transport,
            _maker: PhantomData,
        }
    }
}

impl<Req, Resp, MkE, MkD> Service<ClientContext, ThriftMessage<Req>> for Client<Resp, MkE, MkD>
where
    Req: Send + 'static + EntryMessage,
    Resp: EntryMessage + Send + 'static,
    MkE: MkEncoder + Send + 'static,
    MkD: MkDecoder + Send + 'static,
{
    type Response = Option<ThriftMessage<Resp>>;

    type Error = Error;

    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'cx where Self:'cx;

    fn call<'cx, 's>(
        &'s mut self,
        cx: &'cx mut ClientContext,
        req: ThriftMessage<Req>,
    ) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move {
            let rpc_info = &cx.rpc_info;
            let target = rpc_info.callee().volo_unwrap().address().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "address is required")
            })?;
            let oneway = cx.message_type == TMessageType::OneWay;
            let mut transport = self.make_transport.call(target).await?;
            let resp = transport.send(cx, req, oneway).await;
            if let Ok(None) = resp {
                if !oneway {
                    return Err(crate::Error::Pilota(new_transport_error(
                        TransportErrorKind::EndOfFile,
                        "an unexpected end of file from server",
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
