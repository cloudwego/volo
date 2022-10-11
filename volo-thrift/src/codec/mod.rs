use bytes::{Buf, BytesMut};
use pilota::thrift::{new_protocol_error, ProtocolErrorKind};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::trace;
use volo::util::buf_reader::BufReader;

use self::tt_header::{TTHeaderDecoder, TTHeaderEncoder};
use crate::{
    context::ThriftContext,
    error::Result,
    protocol::{binary::TAsyncBinaryProtocol, rw_ext::WriteExt, TBinaryProtocol},
    EntryMessage, ThriftMessage,
};

pub mod framed;
mod mesh_header;
pub mod tt_header;

mod magic {
    pub const TT_HEADER: u16 = 0x1000;
    #[allow(dead_code)]
    pub const MESH_HEADER: u16 = 0xFFAF;
    #[allow(dead_code)]
    pub const THRIFT_V1_HEADER: u16 = 0x8001;
}

#[derive(Debug, Clone, Copy)]
pub enum CodecType {
    TTHeaderFramed,
    TTHeader,
    Framed,
    Buffered,
}

impl CodecType {
    fn is_ttheader(&self) -> bool {
        matches!(self, CodecType::TTHeaderFramed | CodecType::TTHeader)
    }

    fn is_framed(&self) -> bool {
        matches!(self, CodecType::TTHeaderFramed | CodecType::Framed)
    }

    fn has_length(&self) -> bool {
        self.is_ttheader() || self.is_framed()
    }
}

pub const DEFAULT_TTHEADER_SIZE: usize = 4096; // 4KB should be enough for most headers
pub const MAX_TTHEADER_SIZE: usize = 64 * 1024; // 64KB
pub const DEFAULT_MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16MB
pub const DEFAULT_BUFFER_SIZE: usize = 8192; // 8KB

#[derive(Clone)]
pub struct DefaultEncoder<TT> {
    pub(crate) codec_type: CodecType,
    buffer: BytesMut,
    max_frame_size: usize,
    ttheader_encoder: TT,
}

impl<TT: Send + TTHeaderEncoder> DefaultEncoder<TT> {
    async fn encode<W: AsyncWrite + Unpin + Send, Req: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        writer: &mut W,
        item: ThriftMessage<Req>,
    ) -> Result<()> {
        // we only need to get size here.
        let p = TBinaryProtocol::new(&mut self.buffer);
        let mut size = item.size(&p);
        trace!("[VOLO] encode message size: {}", size);
        if self.codec_type.is_framed() {
            size += 4;
        }
        self.buffer.reserve(DEFAULT_TTHEADER_SIZE + size);

        // 1. encode header.
        if self.codec_type.is_ttheader() {
            let header_size = self.ttheader_encoder.encode(cx, &mut self.buffer, size)?;
            trace!("[VOLO] encode message ttheader size: {}", header_size);
            if header_size > MAX_TTHEADER_SIZE {
                return Err(crate::Error::Pilota(new_protocol_error(
                    ProtocolErrorKind::SizeLimit,
                    "TTHeader size too large".to_string(),
                )));
            }
            if header_size > DEFAULT_TTHEADER_SIZE {
                // we need to enlarge the buffer as ttheader used more than we expected
                self.buffer.reserve(size);
            }
        }

        // TODO: remove remaining capacity check since we can guarentee the buffer is large enough

        // 2. encode framed
        if self.codec_type.is_framed() {
            // not contain self
            size -= 4;
            if size > self.max_frame_size {
                return Err(crate::Error::Pilota(new_protocol_error(
                    ProtocolErrorKind::SizeLimit,
                    format!("Frame of length {} is too large.", size),
                )));
            }
            // encode size
            self.buffer
                .write_u32(size as u32)
                .map_err(Into::<pilota::thrift::error::Error>::into)?;
            trace!("[VOLO] encode message framed header size: {}", size);
            trace!(
                "[VOLO] encode message framed buffer length: {}",
                self.buffer.len()
            );
        }

        // 3. encode item
        let mut p = TBinaryProtocol::new(&mut self.buffer);
        item.encode(&mut p)?;
        trace!("[VOLO] encode message buffer length: {}", self.buffer.len());

        writer.write_all_buf(&mut self.buffer).await?;
        writer.flush().await?;
        self.buffer.clear();
        Ok(())
    }
}

impl<TT> DefaultEncoder<TT> {
    pub fn new(codec_type: CodecType, ttheader_encoder: TT) -> Self {
        let buffer = BytesMut::with_capacity(DEFAULT_BUFFER_SIZE);
        Self {
            codec_type,
            buffer,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            ttheader_encoder,
        }
    }
}

#[derive(Clone)]
pub struct DetectedDecoder<TT> {
    pub(crate) codec_type: Option<CodecType>,
    bytes: BytesMut,
    has_mesh_header: bool,
    ttheader_decoder: TT,
}

impl<TT> DetectedDecoder<TT>
where
    TT: Send + TTHeaderDecoder,
{
    async fn decode<Resp: Send + EntryMessage, R: AsyncRead + Unpin + Send, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<Option<ThriftMessage<Resp>>> {
        let mut buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok(None);
        }
        if let Some(codec_type) = self.codec_type {
            // FIXME: make this zero-copy
            if codec_type.is_ttheader() {
                self.decode_ttheader(cx, reader).await?;
            } else if self.has_mesh_header {
                self.decode_mesh_header(cx, reader).await?;
            }

            if codec_type.is_framed() {
                // we can ignore framed header if we have ttheader
                if codec_type.is_ttheader() {
                    self.bytes.advance(4);
                } else {
                    self.decode_framed(cx, reader).await?;
                }
            }
            return Ok(Some(self.decode_message(cx, reader).await?));
        }

        // detect the protocol
        const HEADER_DETECT_LENGTH: usize = 6;
        buf = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await?;
        let mut codec_type;
        // 1. check if has header: ttheader or mesh header
        if is_ttheader(buf) {
            codec_type = CodecType::TTHeader;
            self.decode_ttheader(cx, reader).await?;
            // data are all in bytes, so we no longer need to read from reader
            // detect if it's framed
            if is_framed(self.bytes.as_ref()) {
                // we have ttheader, so we can just ignore framed header
                codec_type = CodecType::TTHeaderFramed;
                self.bytes.advance(4);
            } else if !is_binary(self.bytes.as_ref()) {
                return Err(crate::Error::Pilota(new_protocol_error(
                    ProtocolErrorKind::BadVersion,
                    "Unknown version".to_string(),
                )));
            }
            self.codec_type = Some(codec_type);
            // 3. decode item
            return Ok(Some(self.decode_message(cx, reader).await?));
        } else if is_mesh_header(buf) {
            self.has_mesh_header = true;
            self.decode_mesh_header(cx, reader).await?;
        }

        let mut buf = reader.fill_buf().await?;
        trace!("[VOLO] decode buf len after header: {}", buf.len());
        buf = reader.fill_buf_at_least(HEADER_DETECT_LENGTH).await?;
        // 2. check if is framed or buffered
        if is_framed(buf) {
            codec_type = CodecType::Framed;
            self.decode_framed(cx, reader).await?;
        } else if is_binary(buf) {
            codec_type = CodecType::Buffered;
        } else {
            return Err(crate::Error::Pilota(new_protocol_error(
                ProtocolErrorKind::BadVersion,
                "Unknown version".to_string(),
            )));
        }
        self.codec_type = Some(codec_type);
        // 3. decode item
        Ok(Some(self.decode_message(cx, reader).await?))
    }
}

impl<TT> DetectedDecoder<TT> {
    pub fn new(ttheader_decoder: TT) -> Self {
        let bytes = BytesMut::with_capacity(DEFAULT_BUFFER_SIZE);
        Self {
            codec_type: None,
            bytes,
            has_mesh_header: false,
            ttheader_decoder,
        }
    }
}
impl<TT: TTHeaderDecoder> DetectedDecoder<TT> {
    pub async fn decode_ttheader<Cx, R: AsyncRead + Unpin>(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<()>
    where
        Cx: ThriftContext,
    {
        let mut size_bytes: [u8; 4] = [0; 4];
        reader.read_exact(&mut size_bytes).await?;
        let size = u32::from_be_bytes(size_bytes) as usize;
        self.bytes.reserve(size);
        unsafe {
            self.bytes.set_len(size);
        }
        reader.read_exact(&mut self.bytes[..size]).await?;
        self.ttheader_decoder.decode(cx, &mut self.bytes)?;

        Ok(())
    }

    pub async fn decode_mesh_header<Cx: ThriftContext, R: AsyncRead + Unpin>(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<()> {
        let mut size_bytes: [u8; 4] = [0; 4];
        reader.read_exact(&mut size_bytes).await?;
        let size = u16::from_be_bytes(size_bytes[2..4].try_into().unwrap()) as usize;
        self.bytes.reserve(size);
        unsafe {
            self.bytes.set_len(size);
        }
        reader.read_exact(&mut self.bytes[..size]).await?;
        mesh_header::decode(&mut self.bytes, cx)
    }

    pub async fn decode_framed<Cx, R: AsyncRead + Unpin>(
        &mut self,
        _cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<()> {
        let mut size_bytes: [u8; 4] = [0; 4];
        reader.read_exact(&mut size_bytes).await?;
        let size = u32::from_be_bytes(size_bytes) as usize;
        self.bytes.reserve(size);
        let index = self.bytes.len();
        unsafe {
            self.bytes.set_len(index + size);
        }
        reader
            .read_exact(&mut self.bytes[index..index + size])
            .await?;
        Ok(())
    }

    pub async fn decode_message<R: AsyncRead + Unpin + Send, Cx: ThriftContext, T: EntryMessage>(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<ThriftMessage<T>> {
        if unsafe { self.codec_type.unwrap_unchecked() }.has_length() {
            // data is in bytes
            let mut protocol = TBinaryProtocol::new(&mut self.bytes);
            let msg = ThriftMessage::<T>::decode(&mut protocol, cx)?;
            self.bytes.clear();
            Ok(msg)
        } else {
            // data in self.reader, so we can directly read from self.reader
            let mut protocol = TAsyncBinaryProtocol::new(reader);
            let msg = ThriftMessage::<T>::decode_async(&mut protocol, cx).await?;
            Ok(msg)
        }
    }
}

fn is_ttheader(buf: &[u8]) -> bool {
    buf[4..6] == [0x10, 0x00]
}

fn is_mesh_header(buf: &[u8]) -> bool {
    buf[0..2] == [0xff, 0xaf]
}

fn is_framed(buf: &[u8]) -> bool {
    buf[4..6] == [0x80, 0x01]
}

fn is_binary(buf: &[u8]) -> bool {
    buf[0..2] == [0x80, 0x01]
}

#[derive(Clone)]
pub struct ServerDecoder<TT>(DetectedDecoder<TT>);

impl<TT> ServerDecoder<TT> {
    pub fn new(tt_decoder: TT) -> Self {
        Self(DetectedDecoder::new(tt_decoder))
    }
}

#[derive(Clone)]
pub struct ServerEncoder<TT> {
    encoder_inited: bool,
    tt_encoder: TT,
    encoder: Option<DefaultEncoder<TT>>,
}

impl<TT> ServerEncoder<TT> {
    pub fn new(tt_encoder: TT) -> Self {
        Self {
            encoder_inited: false,
            tt_encoder,
            encoder: None,
        }
    }
}

#[async_trait::async_trait]
impl<TT: TTHeaderEncoder + Send> Encoder for ServerEncoder<TT> {
    async fn encode<W: AsyncWrite + Unpin + Send, Req: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        writer: &mut W,
        msg: ThriftMessage<Req>,
    ) -> Result<()> {
        // if first call, init encoder
        if !self.encoder_inited {
            let mk_encoder = |codec_type| DefaultEncoder::new(codec_type, self.tt_encoder);
            // get CodecType from cx when first encode
            match cx.extensions().get::<CodecType>() {
                Some(codec_type) => {
                    self.encoder = Some(mk_encoder(*codec_type));
                    // init once
                    self.encoder_inited = true;
                }
                None => self.encoder = Some(mk_encoder(CodecType::TTHeaderFramed)),
            }
        }

        let encoder = self.encoder.as_mut().unwrap();

        encoder.encode(cx, writer, msg).await?;
        trace!(
            "[VOLO] server send encoder buffer len: {}",
            encoder.buffer.len()
        );
        Ok(())
    }
}

#[async_trait::async_trait]
impl<TT: TTHeaderDecoder + Send> Decoder for ServerDecoder<TT> {
    #[inline]
    async fn decode<Resp: Send + EntryMessage, R: AsyncRead + Unpin + Send, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<Option<ThriftMessage<Resp>>> {
        let res = self.0.decode(cx, reader).await;
        // set codec type into extensions
        if let Some(codec_type) = self.0.codec_type {
            cx.extensions_mut().insert(codec_type);
        }
        res
    }
}

pub struct ClientEncoder<TT> {
    encoder: DefaultEncoder<TT>,
}

#[async_trait::async_trait]
impl<TT> Encoder for ClientEncoder<TT>
where
    TT: TTHeaderEncoder,
{
    async fn encode<W: AsyncWrite + Unpin + Send, Req: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        writer: &mut W,
        msg: ThriftMessage<Req>,
    ) -> Result<()> {
        self.encoder.encode(cx, writer, msg).await
    }
}

pub struct ClientDecoder<TT>(DetectedDecoder<TT>);

#[async_trait::async_trait]
impl<TT> Decoder for ClientDecoder<TT>
where
    TT: TTHeaderDecoder,
{
    async fn decode<Resp: Send + EntryMessage, R: AsyncRead + Unpin + Send, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<Option<ThriftMessage<Resp>>> {
        self.0.decode(cx, reader).await
    }
}

#[async_trait::async_trait]
pub trait Encoder {
    async fn encode<W: AsyncWrite + Unpin + Send, Req: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        writer: &mut W,
        msg: ThriftMessage<Req>,
    ) -> Result<()>;
}

#[async_trait::async_trait]
pub trait Decoder {
    async fn decode<Resp: Send + EntryMessage, R: AsyncRead + Unpin + Send, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<Option<ThriftMessage<Resp>>>;
}

pub trait MkEncoder: Clone + Send + Sync + 'static {
    type Target: Encoder + Send + 'static;
    fn mk_encoder(&self, codec_type: Option<CodecType>) -> Self::Target;
}

pub trait MkDecoder: Clone + Send + Sync + 'static {
    type Target: Decoder + Send + 'static;
    fn mk_decoder(&self, codec_type: Option<CodecType>) -> Self::Target;
}

#[derive(Clone, Debug)]
pub struct MakeClientEncoder<TTEncoder> {
    pub(crate) tt_encoder: TTEncoder,
}

#[derive(Clone, Debug)]
pub struct MakeClientDecoder<TTDecoder> {
    pub(crate) tt_decoder: TTDecoder,
}

impl<TTEncoder> MkEncoder for MakeClientEncoder<TTEncoder>
where
    TTEncoder: TTHeaderEncoder,
{
    type Target = ClientEncoder<TTEncoder>;

    fn mk_encoder(&self, codec_type: Option<CodecType>) -> Self::Target {
        ClientEncoder {
            encoder: DefaultEncoder::new(
                codec_type.expect("codec type is require for client encoder"),
                self.tt_encoder,
            ),
        }
    }
}

impl<TTDecoder> MkDecoder for MakeClientDecoder<TTDecoder>
where
    TTDecoder: TTHeaderDecoder,
{
    type Target = ClientDecoder<TTDecoder>;

    fn mk_decoder(&self, _codec_type: Option<CodecType>) -> Self::Target {
        ClientDecoder(DetectedDecoder::new(self.tt_decoder))
    }
}

#[derive(Clone, Debug)]
pub struct MakeServerEncoder<TTEncoder> {
    pub(crate) tt_encoder: TTEncoder,
}

impl<T> MakeServerEncoder<T> {
    pub fn new(tt_encoder: T) -> Self {
        Self { tt_encoder }
    }
}

#[derive(Clone, Debug)]
pub struct MakeServerDecoder<TTDecoder> {
    pub(crate) tt_decoder: TTDecoder,
}

impl<T> MakeServerDecoder<T> {
    pub fn new(tt_decoder: T) -> Self {
        Self { tt_decoder }
    }
}

impl<TTEncoder> MkEncoder for MakeServerEncoder<TTEncoder>
where
    TTEncoder: TTHeaderEncoder,
{
    type Target = ServerEncoder<TTEncoder>;

    fn mk_encoder(&self, _codec_type: Option<CodecType>) -> Self::Target {
        ServerEncoder::new(self.tt_encoder)
    }
}

impl<TTDecoder> MkDecoder for MakeServerDecoder<TTDecoder>
where
    TTDecoder: TTHeaderDecoder,
{
    type Target = ServerDecoder<TTDecoder>;

    fn mk_decoder(&self, _codec_type: Option<CodecType>) -> Self::Target {
        ServerDecoder::new(self.tt_decoder)
    }
}
