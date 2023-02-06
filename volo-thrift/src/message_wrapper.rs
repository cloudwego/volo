use faststr::FastStr;
use pilota::thrift::{Error::Protocol, Message, TAsyncInputProtocol};

use crate::{
    context::{ClientContext, ServerContext, ThriftContext},
    protocol::{
        TInputProtocol, TLengthProtocol, TMessageIdentifier, TMessageType, TOutputProtocol,
    },
    ApplicationError, ApplicationErrorKind, EntryMessage,
};

#[derive(Debug)]
pub struct MessageMeta {
    pub msg_type: TMessageType,
    pub(crate) method: FastStr,
    pub(crate) seq_id: i32,
}

#[derive(Debug)]
pub struct ThriftMessage<M> {
    pub data: Result<M, crate::Error>,
    pub meta: MessageMeta,
}

pub(crate) struct DummyMessage;

#[async_trait::async_trait]
impl EntryMessage for DummyMessage {
    #[inline]
    fn encode<T: TOutputProtocol>(&self, _protocol: &mut T) -> Result<(), crate::Error> {
        unreachable!()
    }

    #[inline]
    fn decode<T: TInputProtocol>(
        _protocol: &mut T,
        _msg_ident: &TMessageIdentifier,
    ) -> Result<Self, crate::Error> {
        unreachable!()
    }

    #[inline]
    async fn decode_async<T: TAsyncInputProtocol>(
        _protocol: &mut T,
        _msg_ident: &TMessageIdentifier,
    ) -> Result<Self, crate::Error> {
        unreachable!()
    }

    fn size<T: TLengthProtocol>(&self, _protocol: &mut T) -> usize {
        unreachable!()
    }
}

impl<M> ThriftMessage<M> {
    #[inline]
    pub fn mk_client_msg(
        cx: &ClientContext,
        msg: Result<M, crate::Error>,
    ) -> Result<Self, crate::Error> {
        let meta = MessageMeta {
            msg_type: cx.message_type,
            method: cx.rpc_info.method.clone().unwrap(),
            seq_id: cx.seq_id,
        };
        Ok(Self { data: msg, meta })
    }

    #[inline]
    pub fn mk_server_resp(
        cx: &ServerContext,
        msg: Result<M, crate::Error>,
    ) -> Result<Self, crate::Error> {
        let meta = MessageMeta {
            msg_type: match msg {
                Ok(_) => TMessageType::Reply,
                Err(_) => TMessageType::Exception,
            },
            method: cx.rpc_info.method.clone().unwrap_or_else(|| "".into()),
            seq_id: cx.seq_id.unwrap_or(0),
        };
        Ok(Self { data: msg, meta })
    }
}

impl<U> ThriftMessage<U>
where
    U: EntryMessage,
{
    pub(crate) fn size<T: TLengthProtocol>(&self, protocol: &mut T) -> usize {
        let ident = TMessageIdentifier::new(
            self.meta.method.clone(),
            self.meta.msg_type,
            self.meta.seq_id,
        );

        match &self.data {
            Ok(inner) => {
                protocol.write_message_begin_len(&ident)
                    + inner.size(protocol)
                    + protocol.write_message_end_len()
            }
            Err(inner) => match inner {
                crate::Error::Application(e) => {
                    protocol.write_message_begin_len(&ident)
                        + e.size(protocol)
                        + protocol.write_message_end_len()
                }
                _ => 0,
            },
        }
    }
}

impl<U> ThriftMessage<U>
where
    U: EntryMessage + Send,
{
    pub(crate) fn encode<T: TOutputProtocol>(&self, protocol: &mut T) -> Result<(), crate::Error> {
        let ident = TMessageIdentifier::new(
            self.meta.method.clone(),
            self.meta.msg_type,
            self.meta.seq_id,
        );
        match &self.data {
            Ok(v) => {
                protocol.write_message_begin(&ident)?;
                v.encode(protocol)?;
            }
            Err(e) => match e {
                crate::Error::Application(e) => {
                    protocol.write_message_begin(&ident)?;
                    e.encode(protocol)?;
                }
                crate::Error::Pilota(e) => match e {
                    Protocol(e) => {
                        protocol.write_message_begin(&ident)?;
                        let e = ApplicationError::new(
                            ApplicationErrorKind::ProtocolError,
                            e.message.clone(),
                        );
                        e.encode(protocol)?;
                    }
                    e => {
                        panic!("should not call send when there is a transport error: {e:?}");
                    }
                },
            },
        }
        protocol.write_message_end()?;
        Ok(())
    }

    pub(crate) fn decode<Cx: ThriftContext, T: TInputProtocol>(
        protocol: &mut T,
        cx: &mut Cx,
    ) -> Result<Self, crate::Error> {
        let msg_ident = protocol.read_message_begin()?;

        cx.handle_decoded_msg_ident(&msg_ident);

        let res = match msg_ident.message_type {
            TMessageType::Exception => Err(crate::Error::Application(Message::decode(protocol)?)),
            _ => Ok(U::decode(protocol, &msg_ident)?),
        };
        protocol.read_message_end()?;
        Ok(ThriftMessage {
            data: res,
            meta: MessageMeta {
                msg_type: msg_ident.message_type,
                method: msg_ident.name,
                seq_id: msg_ident.sequence_number,
            },
        })
    }

    pub(crate) async fn decode_async<Cx: ThriftContext + Send, T: TAsyncInputProtocol>(
        protocol: &mut T,
        cx: &mut Cx,
    ) -> Result<Self, crate::Error> {
        let msg_ident = protocol.read_message_begin().await?;

        cx.handle_decoded_msg_ident(&msg_ident);

        let res = match msg_ident.message_type {
            TMessageType::Exception => Err(crate::Error::Application(
                Message::decode_async(protocol).await?,
            )),
            _ => Ok(U::decode_async(protocol, &msg_ident).await?),
        };
        protocol.read_message_end().await?;
        Ok(ThriftMessage {
            data: res,
            meta: MessageMeta {
                msg_type: msg_ident.message_type,
                method: msg_ident.name,
                seq_id: msg_ident.sequence_number,
            },
        })
    }
}
