use bytes::Bytes;
use linkedbytes::LinkedBytes;
use pilota::thrift::{
    binary::TBinaryProtocol,
    compact::{TCompactInputProtocol, TCompactOutputProtocol},
    DecodeError, DecodeErrorKind, EncodeError, ProtocolError, ProtocolErrorKind,
    TAsyncBinaryProtocol, TAsyncCompactProtocol, TLengthProtocol,
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

/// Use condition to optimize performance(reduce a Box call).
pub struct ProtocolBinary;
pub struct ProtocolApacheCompact;

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
        bytes: &mut Bytes,
    ) -> Result<Option<ThriftMessage<Msg>>, DecodeError> {
        if bytes.len() < HEADER_DETECT_LENGTH {
            // not enough bytes to detect, so return error
            return Err(pilota::thrift::error::DecodeError::new(
                DecodeErrorKind::BadVersion,
                "not enough bytes to detect protocol in thrift codec",
            ));
        }

        // detect protocol
        // TODO: support using protocol from TTHeader
        let protocol = detect(bytes)?;
        // TODO: do we need to check the response protocol at client side?
        match protocol {
            Protocol::Binary => {
                #[cfg(feature = "unsafe-codec")]
                let mut p = unsafe {
                    pilota::thrift::binary_unsafe::TBinaryUnsafeInputProtocol::new(bytes)
                };
                #[cfg(not(feature = "unsafe-codec"))]
                let mut p = TBinaryProtocol::new(bytes, true);
                let msg = ThriftMessage::<Msg>::decode(&mut p, cx)?;
                #[cfg(feature = "unsafe-codec")]
                {
                    use bytes::Buf;
                    use pilota::thrift::TInputProtocol;
                    let index = p.index();
                    p.buf_mut().advance(index);
                }
                cx.conditions_mut().insert::<ProtocolBinary>();
                Ok(Some(msg))
            }
            Protocol::ApacheCompact => {
                let mut p = TCompactInputProtocol::new(bytes);
                let msg = ThriftMessage::<Msg>::decode(&mut p, cx)?;
                cx.conditions_mut().insert::<ProtocolApacheCompact>();
                Ok(Some(msg))
            }
            p => Err(pilota::thrift::error::DecodeError::new(
                DecodeErrorKind::NotImplemented,
                format!("protocol {p:?} is not supported"),
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
    ) -> Result<Option<ThriftMessage<Msg>>, DecodeError> {
        // check if is framed
        let Ok(buf) = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await else {
            cx.stats_mut().record_read_end_at();
            // not enough bytes to detect, so return error
            return Err(pilota::thrift::error::DecodeError::new(
                DecodeErrorKind::BadVersion,
                    "not enough bytes to detect protocol in thrift codec",
                ),
            );
        };

        // detect protocol
        // TODO: support using protocol from TTHeader
        let protocol = detect(buf).map_err(|e| {
            cx.stats_mut().record_read_end_at();
            e
        })?;
        // TODO: do we need to check the response protocol at client side?
        let res = match protocol {
            Protocol::Binary => {
                let mut p = TAsyncBinaryProtocol::new(reader);
                let msg = ThriftMessage::<Msg>::decode_async(&mut p, cx).await?;
                cx.conditions_mut().insert::<ProtocolBinary>();
                Ok(Some(msg))
            }
            Protocol::ApacheCompact => {
                let mut p = TAsyncCompactProtocol::new(reader);
                let msg = ThriftMessage::<Msg>::decode_async(&mut p, cx).await?;
                cx.conditions_mut().insert::<ProtocolApacheCompact>();
                Ok(Some(msg))
            }
            p => Err(pilota::thrift::error::DecodeError::new(
                DecodeErrorKind::NotImplemented,
                format!("protocol {p:?} is not supported"),
            )),
        };
        cx.stats_mut().record_read_end_at();
        res
    }
}

/// Detect protocol according to https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#compatibility
pub fn detect(buf: &[u8]) -> Result<Protocol, ProtocolError> {
    if buf[0] == 0x80 || buf[0] == 0x00 {
        Ok(Protocol::Binary)
    } else if buf[0] == 0x82 {
        // TODO: how do we differ ApacheCompact and FBThriftCompact?
        Ok(Protocol::ApacheCompact)
    } else {
        Err(pilota::thrift::new_protocol_error(
            ProtocolErrorKind::BadVersion,
            format!("unknown protocol, first byte: {}", buf[0]),
        ))
    }
}

impl ZeroCopyEncoder for ThriftCodec {
    fn encode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        linked_bytes: &mut LinkedBytes,
        msg: ThriftMessage<Msg>,
    ) -> Result<(), EncodeError> {
        // for the client side, the match expression will always be `&self.protocol`
        // TODO: use the protocol in TTHeader?
        let mut protocol = self.protocol;
        if cx.conditions().contains::<ProtocolBinary>() {
            protocol = Protocol::Binary;
        } else if cx.conditions().contains::<ProtocolApacheCompact>() {
            protocol = Protocol::ApacheCompact;
        }
        match protocol {
            Protocol::Binary => {
                #[cfg(feature = "unsafe-codec")]
                let buf = unsafe {
                    let l = linked_bytes.bytes_mut().len();
                    std::slice::from_raw_parts_mut(
                        linked_bytes.bytes_mut().as_mut_ptr().offset(l as isize),
                        linked_bytes.bytes_mut().capacity() - l,
                    )
                };
                #[cfg(feature = "unsafe-codec")]
                let mut p = unsafe {
                    pilota::thrift::binary_unsafe::TBinaryUnsafeOutputProtocol::new(
                        linked_bytes,
                        buf,
                        true,
                    )
                };
                #[cfg(not(feature = "unsafe-codec"))]
                let mut p = TBinaryProtocol::new(linked_bytes, true);
                msg.encode(&mut p)?;
                #[cfg(feature = "unsafe-codec")]
                {
                    use bytes::BufMut;
                    use pilota::thrift::TOutputProtocol;
                    let index = p.index();
                    unsafe {
                        p.buf_mut().bytes_mut().advance_mut(index);
                    }
                }
                Ok(())
            }
            Protocol::ApacheCompact => {
                let mut p = TCompactOutputProtocol::new(linked_bytes, true);
                msg.encode(&mut p)?;
                Ok(())
            }
            p => Err(pilota::thrift::error::EncodeError::new(
                ProtocolErrorKind::NotImplemented,
                format!("protocol {p:?} is not supported"),
            )),
        }
    }

    fn size<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: &ThriftMessage<Msg>,
    ) -> Result<(usize, usize), EncodeError> {
        // for the client side, the match expression will always be `&self.protocol`
        // TODO: use the protocol in TTHeader?
        let mut protocol = self.protocol;
        if cx.conditions().contains::<ProtocolBinary>() {
            protocol = Protocol::Binary;
        } else if cx.conditions().contains::<ProtocolApacheCompact>() {
            protocol = Protocol::ApacheCompact;
        }
        match protocol {
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
            p => Err(pilota::thrift::error::EncodeError::new(
                ProtocolErrorKind::NotImplemented,
                format!("protocol {p:?} is not supported"),
            )),
        }
    }
}
