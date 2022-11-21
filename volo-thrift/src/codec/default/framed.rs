use bytes::{Buf, BytesMut};
use linkedbytes::LinkedBytes;
use pilota::thrift::rw_ext::WriteExt;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt};
use tracing::trace;
use volo::{context::Role, util::buf_reader::BufReader};

use super::{MakeZeroCopyCodec, ZeroCopyDecoder, ZeroCopyEncoder};
use crate::{context::ThriftContext, EntryMessage, ThriftMessage};

/// Default limit according to thrift spec.
/// https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#framed-vs-unframed-transport
pub const DEFAULT_MAX_FRAME_SIZE: i32 = 16 * 1024 * 1024; // 16MB

/// [`MakeFramedCodec`] implements [`MakeZeroCopyCodec`] to create [`FramedEncoder`] and
/// [`FramedDecoder`].
#[derive(Clone)]
pub struct MakeFramedCodec<Inner: MakeZeroCopyCodec> {
    inner: Inner,
    max_frame_size: i32,
}

impl<Inner: MakeZeroCopyCodec> MakeFramedCodec<Inner> {
    pub fn new(inner: Inner) -> Self {
        Self {
            inner,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        }
    }

    pub fn with_max_frame_size(mut self, max_frame_size: i32) -> Self {
        self.max_frame_size = max_frame_size;
        self
    }
}

impl<Inner: MakeZeroCopyCodec> MakeZeroCopyCodec for MakeFramedCodec<Inner> {
    type Encoder = FramedEncoder<Inner::Encoder>;

    type Decoder = FramedDecoder<Inner::Decoder>;

    fn make_codec(&self) -> (Self::Encoder, Self::Decoder) {
        let (encoder, decoder) = self.inner.make_codec();
        (
            FramedEncoder::new(encoder, self.max_frame_size),
            FramedDecoder::new(decoder, self.max_frame_size),
        )
    }
}

/// This is used to tell the encoder to encode framed header at server side.
pub struct HasFramed(bool);

#[derive(Clone)]
pub struct FramedDecoder<D: ZeroCopyDecoder> {
    inner: D,
    max_frame_size: i32,
}

impl<D: ZeroCopyDecoder> FramedDecoder<D> {
    pub fn new(inner: D, max_frame_size: i32) -> Self {
        Self {
            inner,
            max_frame_size,
        }
    }
}

/// 4-bytes length + 1-byte protocol id
/// https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#compatibility
pub const HEADER_DETECT_LENGTH: usize = 5;

#[async_trait::async_trait]
impl<D> ZeroCopyDecoder for FramedDecoder<D>
where
    D: ZeroCopyDecoder,
{
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        mut bytes: BytesMut,
    ) -> crate::Result<Option<ThriftMessage<Msg>>> {
        if bytes.len() < HEADER_DETECT_LENGTH {
            // not enough bytes to detect, must not be Framed, so just forward to inner
            return self.inner.decode(cx, bytes);
        }

        if is_framed(&bytes[..HEADER_DETECT_LENGTH]) {
            let size = bytes.get_i32();
            check_framed_size(size, self.max_frame_size)?;
            // set has framed flag
            cx.extensions_mut().insert(HasFramed(true));
        }
        // decode inner
        self.inner.decode(cx, bytes)
    }

    async fn decode_async<
        Msg: Send + EntryMessage,
        Cx: ThriftContext,
        R: AsyncRead + Unpin + Send + Sync,
    >(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> crate::Result<Option<ThriftMessage<Msg>>> {
        // check if is framed
        if let Ok(buf) = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await {
            if is_framed(buf) {
                // read all the data out, and call inner decode instead of decode_async
                let size = i32::from_be_bytes(buf[0..4].try_into().unwrap());
                reader.consume(4);
                check_framed_size(size, self.max_frame_size)?;

                let mut bytes = BytesMut::with_capacity(size as usize);
                unsafe {
                    bytes.set_len(size as usize);
                }
                reader.read_exact(&mut bytes[..size as usize]).await?;

                // set has framed flag
                cx.extensions_mut().insert(HasFramed(true));
                // decode inner
                self.inner.decode(cx, bytes)
            } else {
                // no Framed, just forward to inner decoder
                self.inner.decode_async(cx, reader).await
            }
        } else {
            return self.inner.decode_async(cx, reader).await;
        }
    }
}

/// Detect protocol according to https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#compatibility
pub fn is_framed(buf: &[u8]) -> bool {
    // binary
    (buf[4] == 0x80 || buf[4] == 0x00)
    ||
    // compact
    buf[4] == 0x82
}

#[derive(Clone)]
pub struct FramedEncoder<E: ZeroCopyEncoder> {
    inner: E,
    inner_size: i32, // cache inner size
    max_frame_size: i32,
}

impl<E: ZeroCopyEncoder> FramedEncoder<E> {
    pub fn new(inner: E, max_frame_size: i32) -> Self {
        Self {
            inner,
            inner_size: 0,
            max_frame_size,
        }
    }
}

pub const FRAMED_HEADER_SIZE: usize = 4;

impl<E> ZeroCopyEncoder for FramedEncoder<E>
where
    E: ZeroCopyEncoder,
{
    fn encode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        linked_bytes: &mut LinkedBytes,
        msg: ThriftMessage<Msg>,
    ) -> crate::Result<()> {
        let dst = linked_bytes.bytes_mut();
        // only encode framed if role is client or server has detected framed in decode
        if cx.rpc_info().role() == Role::Client
            || cx
                .extensions()
                .get::<HasFramed>()
                .unwrap_or(&HasFramed(false))
                .0
        {
            // encode framed first
            dst.write_i32(self.inner_size)
                .map_err(Into::<pilota::thrift::error::Error>::into)?;
            trace!(
                "[VOLO] encode message framed header size: {}",
                self.inner_size
            );
        }
        self.inner.encode(cx, linked_bytes, msg)
    }

    fn size<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: &ThriftMessage<Msg>,
    ) -> crate::Result<usize> {
        self.inner_size = self.inner.size(cx, msg)? as i32;
        check_framed_size(self.inner_size, self.max_frame_size)?;
        // only calc framed size if role is client or server has detected framed in decode
        if cx.rpc_info().role() == Role::Client
            || cx
                .extensions()
                .get::<HasFramed>()
                .unwrap_or(&HasFramed(false))
                .0
        {
            Ok(self.inner_size as usize + FRAMED_HEADER_SIZE)
        } else {
            Ok(self.inner_size as usize)
        }
    }
}

/// Checks the framed size according to thrift spec.
/// https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#framed-vs-unframed-transport
pub fn check_framed_size(size: i32, max_frame_size: i32) -> Result<(), crate::Error> {
    if size > max_frame_size {
        return Err(crate::Error::Pilota(pilota::thrift::new_protocol_error(
            pilota::thrift::ProtocolErrorKind::SizeLimit,
            format!(
                "frame size {} exceeds max frame size {}",
                size, max_frame_size
            ),
        )));
    }
    if size < 0 {
        return Err(crate::Error::Pilota(pilota::thrift::new_protocol_error(
            pilota::thrift::ProtocolErrorKind::NegativeSize,
            format!("frame size {} is negative", size,),
        )));
    }
    Ok(())
}
