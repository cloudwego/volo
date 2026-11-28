use std::{future::Future, sync::Arc};

use bytes::{Buf, Bytes};
pub use pilota::thrift::Message;
use pilota::thrift::{
    ProtocolException, TAsyncInputProtocol, TInputProtocol, TLengthProtocol, TMessageIdentifier,
    TOutputProtocol, ThriftException,
};
use tokio::io::AsyncRead;
use volo::util::buf_reader::BufReader;

use crate::{ThriftMessage, codec::default::thrift::Protocol, context::ThriftContext};

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

    /// Context-aware decoding used by a shared, typed service directory.
    #[doc(hidden)]
    fn decode_with_context<Cx: ThriftContext>(
        _bytes: &mut Bytes,
        _protocol: Protocol,
        _cx: &mut Cx,
    ) -> Option<Result<ThriftMessage<Self>, ThriftException>> {
        None
    }

    #[doc(hidden)]
    fn decode_async_with_context<Cx: ThriftContext, R: AsyncRead + Unpin + Send>(
        _reader: &mut BufReader<R>,
        _protocol: Protocol,
        _cx: &mut Cx,
    ) -> impl Future<Output = Option<Result<ThriftMessage<Self>, ThriftException>>> + Send {
        async { None }
    }

    #[doc(hidden)]
    fn dynamic_encoder(&self) -> Option<&dyn crate::server::routed::MessageEncoder> {
        None
    }
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

impl EntryMessage for Bytes {
    fn encode<T: TOutputProtocol>(&self, protocol: &mut T) -> Result<(), ThriftException> {
        protocol.write_bytes_without_len(self.clone())
    }

    fn decode<T: TInputProtocol>(
        protocol: &mut T,
        _msg_ident: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        let ptr = protocol.buf().chunk().as_ptr();
        let len = protocol.buf().remaining();
        let buf = protocol.get_bytes(Some(ptr), len)?;

        Ok(buf)
    }

    async fn decode_async<T: TAsyncInputProtocol>(
        _protocol: &mut T,
        _msg_ident: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        Err(ThriftException::Protocol(ProtocolException::new(
            pilota::thrift::ProtocolExceptionKind::NotImplemented,
            "Binary response decode is not supported for pure Buffered protocol since we don't \
             know the length of the message",
        )))
    }

    fn size<T: TLengthProtocol>(&self, _protocol: &mut T) -> usize {
        self.as_ref().len()
    }
}
