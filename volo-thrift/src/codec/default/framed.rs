use bytes::{Buf, BytesMut};
use linkedbytes::LinkedBytes;
use pilota::thrift::{rw_ext::WriteExt, DecodeError, EncodeError, ProtocolError};
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
    buffer: BytesMut,
    max_frame_size: i32,
}

impl<D: ZeroCopyDecoder> FramedDecoder<D> {
    pub fn new(inner: D, max_frame_size: i32) -> Self {
        Self {
            inner,
            max_frame_size,
            buffer: BytesMut::new(),
        }
    }
}

/// 4-bytes length + 2-byte protocol id
/// https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#compatibility
pub const HEADER_DETECT_LENGTH: usize = 6;

#[async_trait::async_trait]
impl<D> ZeroCopyDecoder for FramedDecoder<D>
where
    D: ZeroCopyDecoder,
{
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        bytes: &mut BytesMut,
    ) -> Result<Option<ThriftMessage<Msg>>, DecodeError> {
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
    ) -> Result<Option<ThriftMessage<Msg>>, DecodeError> {
        // check if is framed
        if let Ok(buf) = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await {
            if is_framed(buf) {
                // read all the data out, and call inner decode instead of decode_async
                let size = i32::from_be_bytes(buf[0..4].try_into().unwrap());
                cx.stats_mut().set_read_size(size as usize + 4);

                reader.consume(4);
                check_framed_size(size, self.max_frame_size)?;

                self.buffer.clear();
                self.buffer.reserve(size as usize);
                unsafe {
                    self.buffer.set_len(size as usize);
                }
                reader.read_exact(&mut self.buffer[..size as usize]).await?;
                cx.stats_mut().record_read_end_at();

                // set has framed flag
                cx.extensions_mut().insert(HasFramed(true));
                // decode inner
                self.inner.decode(cx, &mut self.buffer)
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
    // in practice, using (buf[4] == 0x80 || buf[4] == 0x00) according to the spec is likely to be
    // wrong
    (buf[4..6] == [0x80, 0x01])
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
    ) -> Result<(), EncodeError> {
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
            dst.write_i32(self.inner_size)?;
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
    ) -> Result<(usize, usize), EncodeError> {
        let (real_size, malloc_size) = self.inner.size(cx, msg)?;
        self.inner_size = real_size as i32;
        // only calc framed size if role is client or server has detected framed in decode
        if cx.rpc_info().role() == Role::Client
            || cx
                .extensions()
                .get::<HasFramed>()
                .unwrap_or(&HasFramed(false))
                .0
        {
            check_framed_size(self.inner_size, self.max_frame_size)?;
            Ok((
                real_size + FRAMED_HEADER_SIZE,
                malloc_size + FRAMED_HEADER_SIZE,
            ))
        } else {
            Ok((real_size, malloc_size))
        }
    }
}

/// Checks the framed size according to thrift spec.
/// https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#framed-vs-unframed-transport
pub fn check_framed_size(size: i32, max_frame_size: i32) -> Result<(), ProtocolError> {
    if size > max_frame_size {
        return Err(pilota::thrift::new_protocol_error(
            pilota::thrift::ProtocolErrorKind::SizeLimit,
            format!("frame size {size} exceeds max frame size {max_frame_size}"),
        ));
    }
    if size < 0 {
        return Err(pilota::thrift::new_protocol_error(
            pilota::thrift::ProtocolErrorKind::NegativeSize,
            format!("frame size {size} is negative"),
        ));
    }
    Ok(())
}
