use bytes::BytesMut;
use linkedbytes::LinkedBytes;
use pilota::thrift::{
    binary::TBinaryProtocol,
    compact::{TCompactInputProtocol, TCompactOutputProtocol},
    ProtocolErrorKind, TAsyncBinaryProtocol, TAsyncCompactProtocol, TLengthProtocol,
};
use tokio::io::AsyncRead;
use volo::util::buf_reader::BufReader;

use super::{MakeZeroCopyCodec, ZeroCopyDecoder, ZeroCopyEncoder};
use crate::{context::ThriftContext, EntryMessage, ThriftMessage};

/// [`MakeThriftCodec`] implements [`MakeZeroCopyCodec`] to create [`ThriftCodec`].
#[derive(Debug, Clone, Copy)]
pub struct MakeThriftCodec {
    protocol: Protocol,
}

impl MakeThriftCodec {
    pub fn new() -> Self {
        Self {
            protocol: Protocol::Binary,
        }
    }

    /// Whether to use thrift multiplex protocol.
    ///
    /// When the multiplexed protocol is used, the name contains the service name,
    /// a colon : and the method name. The multiplexed protocol is not compatible
    /// with other protocols.
    ///
    /// Spec: https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md
    ///
    /// This is unimplemented yet.
    // pub fn with_multiplex(mut self, multiplex: bool) -> Self {
    //     self.multiplex = multiplex;
    //     self
    // }

    /// The `protocol` only takes effect at client side. The server side will auto detect the
    /// protocol.
    pub fn with_protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = protocol;
        self
    }
}

impl Default for MakeThriftCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl MakeZeroCopyCodec for MakeThriftCodec {
    type Encoder = ThriftCodec;

    type Decoder = ThriftCodec;

    fn make_codec(&self) -> (Self::Encoder, Self::Decoder) {
        let codec = ThriftCodec::new(self.protocol);
        (codec, codec)
    }
}

/// This is used to tell the encoder which protocol is used.
#[derive(Debug, Clone, Copy)]
pub enum Protocol {
    Binary,
    ApacheCompact,
    FBThriftCompact,
}

/// 1-byte protocol id
/// https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#compatibility
pub const HEADER_DETECT_LENGTH: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct ThriftCodec {
    protocol: Protocol,
}

impl ThriftCodec {
    /// The `protocol` only takes effect at client side. The server side will auto detect the
    /// protocol.
    pub fn new(protocol: Protocol) -> Self {
        Self { protocol }
    }
}

impl Default for ThriftCodec {
    fn default() -> Self {
        Self::new(Protocol::Binary)
    }
}

#[async_trait::async_trait]
impl ZeroCopyDecoder for ThriftCodec {
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        mut bytes: BytesMut,
    ) -> crate::Result<Option<ThriftMessage<Msg>>> {
        if bytes.len() < HEADER_DETECT_LENGTH {
            // not enough bytes to detect, so return error
            return Err(crate::Error::Pilota(
                pilota::thrift::error::new_protocol_error(
                    ProtocolErrorKind::BadVersion,
                    "not enough bytes to detect protocol in thrift codec",
                ),
            ));
        }

        // detect protocol
        // TODO: support using protocol from TTHeader
        let protocol = detect(&bytes)?;
        // TODO: do we need to check the response protocol at client side?
        match protocol {
            Protocol::Binary => {
                let mut p = TBinaryProtocol::new(&mut bytes, true);
                let msg = ThriftMessage::<Msg>::decode(&mut p, cx)?;
                cx.extensions_mut().insert(protocol);
                Ok(Some(msg))
            }
            Protocol::ApacheCompact => {
                let mut p = TCompactInputProtocol::new(&mut bytes);
                let msg = ThriftMessage::<Msg>::decode(&mut p, cx)?;
                cx.extensions_mut().insert(protocol);
                Ok(Some(msg))
            }
            p => Err(crate::Error::Pilota(
                pilota::thrift::error::new_protocol_error(
                    ProtocolErrorKind::NotImplemented,
                    format!("protocol {:?} is not supported", p),
                ),
            )),
        }
    }

    async fn decode_async<
        Msg: Send + EntryMessage,
        Cx: ThriftContext,
        R: AsyncRead + Unpin + Send,
    >(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> crate::Result<Option<ThriftMessage<Msg>>> {
        // check if is framed
        let Ok(buf) = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await else {
            // not enough bytes to detect, so return error
            return Err(crate::Error::Pilota(
                pilota::thrift::error::new_protocol_error(
                    ProtocolErrorKind::BadVersion,
                    "not enough bytes to detect protocol in thrift codec",
                ),
            ));
        };

        // detect protocol
        // TODO: support using protocol from TTHeader
        let protocol = detect(buf)?;
        // TODO: do we need to check the response protocol at client side?
        match protocol {
            Protocol::Binary => {
                let mut p = TAsyncBinaryProtocol::new(reader);
                let msg = ThriftMessage::<Msg>::decode_async(&mut p, cx).await?;
                cx.extensions_mut().insert(protocol);
                Ok(Some(msg))
            }
            Protocol::ApacheCompact => {
                let mut p = TAsyncCompactProtocol::new(reader);
                let msg = ThriftMessage::<Msg>::decode_async(&mut p, cx).await?;
                cx.extensions_mut().insert(protocol);
                Ok(Some(msg))
            }
            p => Err(crate::Error::Pilota(
                pilota::thrift::error::new_protocol_error(
                    ProtocolErrorKind::NotImplemented,
                    format!("protocol {:?} is not supported", p),
                ),
            )),
        }
    }
}

/// Detect protocol according to https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#compatibility
pub fn detect(buf: &[u8]) -> Result<Protocol, crate::Error> {
    if buf[0] == 0x80 || buf[0] == 0x00 {
        Ok(Protocol::Binary)
    } else if buf[0] == 0x82 {
        // TODO: how do we differ ApacheCompact and FBThriftCompact?
        Ok(Protocol::ApacheCompact)
    } else {
        Err(crate::Error::Pilota(pilota::thrift::new_protocol_error(
            ProtocolErrorKind::BadVersion,
            format!("unknown protocol, first byte: {}", buf[0]),
        )))
    }
}

impl ZeroCopyEncoder for ThriftCodec {
    fn encode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        linked_bytes: &mut LinkedBytes,
        msg: ThriftMessage<Msg>,
    ) -> crate::Result<()> {
        // for the client side, the match expression will always be `&self.protocol`
        // TODO: use the protocol in TTHeader?
        match cx.extensions().get::<Protocol>().unwrap_or(&self.protocol) {
            Protocol::Binary => {
                let mut p = TBinaryProtocol::new(linked_bytes, true);
                msg.encode(&mut p)?;
                Ok(())
            }
            Protocol::ApacheCompact => {
                let mut p = TCompactOutputProtocol::new(linked_bytes, true);
                msg.encode(&mut p)?;
                Ok(())
            }
            p => Err(crate::Error::Pilota(
                pilota::thrift::error::new_protocol_error(
                    ProtocolErrorKind::NotImplemented,
                    format!("protocol {:?} is not supported", p),
                ),
            )),
        }
    }

    fn size<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: &ThriftMessage<Msg>,
    ) -> crate::Result<(usize, usize)> {
        // for the client side, the match expression will always be `&self.protocol`
        // TODO: use the protocol in TTHeader?
        match cx.extensions().get::<Protocol>().unwrap_or(&self.protocol) {
            Protocol::Binary => {
                let mut p = TBinaryProtocol::new((), true);
                let real_size = msg.size(&mut p);
                let malloc_size = real_size - p.zero_copy_len();
                Ok((real_size, malloc_size))
            }
            Protocol::ApacheCompact => {
                let mut p = TCompactOutputProtocol::new((), true);
                let real_size = msg.size(&mut p);
                let malloc_size = real_size - p.zero_copy_len();
                Ok((real_size, malloc_size))
            }
            p => Err(crate::Error::Pilota(
                pilota::thrift::error::new_protocol_error(
                    ProtocolErrorKind::NotImplemented,
                    format!("protocol {:?} is not supported", p),
                ),
            )),
        }
    }
}
