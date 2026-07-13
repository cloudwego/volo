use std::{io, marker::PhantomData};

use motore::service::{Service, UnaryService};
use volo::net::{Address, dial::MakeTransport};

use crate::{
    ClientError, EntryMessage, ThriftMessage,
    codec::MakeCodec,
    context::ClientContext,
    protocol::TMessageType,
    transport::{
        dial::TransportAcquirer,
        multiplex::thrift_transport::ThriftTransport,
        pool::{Config, Ver},
    },
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

    async fn call(&self, target: Address) -> Result<Self::Response, Self::Error> {
        #[cfg(feature = "shmipc")]
        if target.is_shmipc() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "shmipc does not support multiplex",
            ));
        }
        let make_transport = self.make_transport.clone();
        let (rh, wh) = make_transport.make_transport(target.clone()).await?;
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
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
    Resp: EntryMessage + Send + 'static,
{
    acquirer: TransportAcquirer<MakeClientTransport<MkT, MkC, Resp>>,
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
            acquirer: self.acquirer.clone(),
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
        let acquirer = TransportAcquirer::new(make_transport, pool_cfg);
        Client {
            acquirer,
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

    type Error = ClientError;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: ThriftMessage<Req>,
    ) -> Result<Self::Response, Self::Error> {
        let oneway = cx.message_type == TMessageType::OneWay;
        // Acquire happens before `send`; the candidate walk and stats live in the acquirer.
        // A multiplex acquire always yields a pooled lease (shmipc candidates are skipped).
        let acquired = self.acquirer.acquire(cx, Ver::Multiplex).await?;
        let resp = acquired.transport().send(cx, req, oneway).await;
        if let Ok(None) = resp {
            if !oneway {
                return Err(ClientError::Transport(
                    pilota::thrift::TransportException::from(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("an unexpected end of file from server, cx: {cx:?}"),
                    )),
                ));
            }
        }
        if cx.transport.should_reuse && resp.is_ok() {
            acquired.reuse().await;
        }
        resp
    }
}
