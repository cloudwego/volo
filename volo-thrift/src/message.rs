use std::{future::Future, sync::Arc};

pub use pilota::thrift::Message;
use pilota::thrift::{
    TAsyncInputProtocol, TInputProtocol, TLengthProtocol, TMessageIdentifier, TOutputProtocol,
    ThriftException,
};

pub trait EntryMessage: Sized + Send {
    fn encode<T: TOutputProtocol>(&self, protocol: &mut T) -> Result<(), ThriftException>;

    fn decode<T: TInputProtocol>(
        protocol: &mut T,
        msg_ident: &TMessageIdentifier,
    ) -> Result<Self, ThriftException>;

    fn decode_async<T: TAsyncInputProtocol>(
        protocol: &mut T,
        msg_ident: &TMessageIdentifier,
    ) -> impl Future<Output = Result<Self, ThriftException>> + Send;

    fn size<T: TLengthProtocol>(&self, protocol: &mut T) -> usize;
}

impl<Message> EntryMessage for Arc<Message>
where
    Message: EntryMessage + Sync,
{
    #[inline]
    fn encode<T: TOutputProtocol>(&self, protocol: &mut T) -> Result<(), ThriftException> {
        (**self).encode(protocol)
    }

    #[inline]
    fn decode<T: TInputProtocol>(
        protocol: &mut T,
        msg_ident: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        Message::decode(protocol, msg_ident).map(Arc::new)
    }

    #[inline]
    async fn decode_async<T: TAsyncInputProtocol>(
        protocol: &mut T,
        msg_ident: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        Message::decode_async(protocol, msg_ident)
            .await
            .map(Arc::new)
    }

    #[inline]
    fn size<T: TLengthProtocol>(&self, protocol: &mut T) -> usize {
        (**self).size(protocol)
    }
}
