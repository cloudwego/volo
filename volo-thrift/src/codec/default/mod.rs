//! This mod contains the default implementation of codec.
//!
//! We use some internal traits such as [`ZeroCopyEncoder`] and [`ZeroCopyDecoder`] to
//! make the implementation more flexible, which is not desired to be used by others, so
//! we don't provide backward compatibility for them.
//!
//! The main entrypoint is [`DefaultMakeCodec`] which receives [`MakeZeroCopyCodec`], and
//! then creates [`DefaultEncoder`] and [`DefaultDecoder`].
//!
//! [`DefaultMakeCodec`] implements [`MakeCodec`] which is used by [`Server`] and [`Client`].
//!
//! We make this mod public for those who want to implement their own codec and want to
//! reuse some of the components.
//!
//! The default codec contains some private protocols, such as [`TTHeader`][TTHeader], which can
//! only be used between [`Volo`][Volo] and [`Kitex`][Kitex] services (currently). If you want to
//! use the standard thrift transport protocol, you can disable [`TTHeader`][TTHeader] and use
//! [`Framed`][Framed] instead.
//!
//! Currently, the default codec protocol is `TTHeader<Framed<Binary>>`.
//!
//! Note: The default implementation of codec assumes that the transport and protocol won't change
//! across a connection.
//!
//! [Volo]: https://github.com/cloudwego/volo
//! [Kitex]: https://github.com/cloudwego/kitex
//! [TTHeader]: https://www.cloudwego.io/docs/kitex/reference/transport_protocol_ttheader/
//! [Framed]: https://github.com/apache/thrift/blob/master/doc/specs/thrift-rpc.md#framed-vs-unframed-transport
use bytes::BytesMut;
use linkedbytes::LinkedBytes;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tracing::trace;
use volo::util::buf_reader::BufReader;

use self::{framed::MakeFramedCodec, thrift::MakeThriftCodec, ttheader::MakeTTHeaderCodec};
use super::{Decoder, Encoder, MakeCodec};
use crate::{context::ThriftContext, EntryMessage, Result, ThriftMessage};

pub mod framed;
pub mod thrift;
pub mod ttheader;
// mod mesh_header;

/// [`ZeroCopyEncoder`] tries to encode a message without copying large data taking the advantage of
/// [`LinkedBytes`], which can insert a [`Bytes`] into the middle of a [`BytesMut`] and uses writev.
///
/// The recommended length threshold to use `LinkedBytes::insert` is 4KB.
pub trait ZeroCopyEncoder: Send + Sync + 'static {
    /// [`encode`] can rely on the `cx` to get some information such as the protocol detected by
    /// the decoder.
    fn encode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        linked_bytes: &mut LinkedBytes,
        msg: ThriftMessage<Msg>,
    ) -> Result<()>;

    /// [`size`] should return the exact size of the encoded message, as we will pre-allocate
    /// a buffer for the encoded message.
    ///
    /// To avoid the overhead of calculating the size again in the [`encode`] method, the
    /// implementation can cache the size in the struct.
    ///
    /// The returned value is (real_size, recommended_malloc_size).
    fn size<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: &ThriftMessage<Msg>,
    ) -> Result<(usize, usize)>;
}

/// [`ZeroCopyDecoder`] tries to decode a message without copying large data, so the [`BytesMut`] in
/// the [`decode`] method is not designed to be reused, and the implementation can use
/// `BytesMut::freeze` to get a [`Bytes`] and hand it to the user directly.
#[async_trait::async_trait]
pub trait ZeroCopyDecoder: Send + Sync + 'static {
    /// If the outer decoder is framed, it can reads all the payload into a [`BytesMut`] and
    /// call this function for better performance.
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        bytes: BytesMut,
    ) -> Result<Option<ThriftMessage<Msg>>>;

    /// The [`DefaultDecoder`] will always call `decode_async`, so the most outer decoder
    /// must implement this function.
    async fn decode_async<
        Msg: Send + EntryMessage,
        Cx: ThriftContext,
        R: AsyncRead + Unpin + Send + Sync,
    >(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> Result<Option<ThriftMessage<Msg>>>;
}

/// [`MakeZeroCopyCodec`] is used to create a [`ZeroCopyEncoder`] and a [`ZeroCopyDecoder`].
///
/// This is the main entrypoint for [`DefaultMakeCodec`].
pub trait MakeZeroCopyCodec: Clone + Send + 'static {
    type Encoder: ZeroCopyEncoder;
    type Decoder: ZeroCopyDecoder;

    fn make_codec(&self) -> (Self::Encoder, Self::Decoder);
}

pub struct DefaultEncoder<E, W> {
    encoder: E,
    writer: W,
    linked_bytes: LinkedBytes,
}

#[async_trait::async_trait]
impl<E: ZeroCopyEncoder, W: AsyncWrite + Unpin + Send + Sync + 'static> Encoder
    for DefaultEncoder<E, W>
{
    async fn encode<Req: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: ThriftMessage<Req>,
    ) -> Result<()> {
        // first, we need to get the size of the message
        let (real_size, malloc_size) = self.encoder.size(cx, &msg)?;
        trace!(
            "[VOLO] codec encode message real size: {}, malloc size: {}",
            real_size,
            malloc_size
        );
        // then we reserve the size of the message in the linked bytes
        self.linked_bytes.reserve(malloc_size);
        // after that, we encode the message into the linked bytes
        self.encoder.encode(cx, &mut self.linked_bytes, msg)?;
        self.linked_bytes
            .write_all_vectored(&mut self.writer)
            .await?;
        self.writer.flush().await?;
        // finally, don't forget to reset the linked bytes
        self.linked_bytes.reset();
        Ok(())
    }
}

pub struct DefaultDecoder<D, R> {
    decoder: D,
    reader: BufReader<R>,
}

#[async_trait::async_trait]
impl<D: ZeroCopyDecoder, R: AsyncRead + Unpin + Send + Sync + 'static> Decoder
    for DefaultDecoder<D, R>
{
    async fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<ThriftMessage<Msg>>> {
        // just to check if we have reached EOF
        if self.reader.fill_buf().await?.is_empty() {
            return Ok(None);
        }
        // simply call the inner `decode_async`
        self.decoder.decode_async(cx, &mut self.reader).await
    }
}

/// `MkZC` is a shorthand for [`MakeZeroCopyCodec`].
#[derive(Clone)]
pub struct DefaultMakeCodec<MkZC: MakeZeroCopyCodec> {
    make_zero_copy_codec: MkZC,
}

impl DefaultMakeCodec<MakeFramedCodec<MakeThriftCodec>> {
    pub fn framed() -> Self {
        DefaultMakeCodec::new(framed::MakeFramedCodec::new(
            thrift::MakeThriftCodec::default(),
        ))
    }
}

impl DefaultMakeCodec<MakeTTHeaderCodec<MakeFramedCodec<MakeThriftCodec>>> {
    pub fn ttheader_framed() -> Self {
        DefaultMakeCodec::new(ttheader::MakeTTHeaderCodec::new(
            framed::MakeFramedCodec::new(thrift::MakeThriftCodec::default()),
        ))
    }
}

impl DefaultMakeCodec<MakeThriftCodec> {
    pub fn buffered() -> Self {
        DefaultMakeCodec::new(thrift::MakeThriftCodec::default())
    }
}

impl<MkZC: MakeZeroCopyCodec> DefaultMakeCodec<MkZC> {
    /// `make_zero_copy_codec` should implement [`MakeZeroCopyCodec`], which will be used to create
    /// the inner [`ZeroCopyEncoder`] and [`ZeroCopyDecoder`].
    pub fn new(make_zero_copy_codec: MkZC) -> Self {
        Self {
            make_zero_copy_codec,
        }
    }
}

impl Default for DefaultMakeCodec<MakeTTHeaderCodec<MakeFramedCodec<MakeThriftCodec>>> {
    fn default() -> Self {
        // TTHeader<Framed<Thrift>>
        Self::new(ttheader::MakeTTHeaderCodec::new(
            framed::MakeFramedCodec::new(thrift::MakeThriftCodec::default()),
        ))
    }
}

impl<MkZC, R, W> MakeCodec<R, W> for DefaultMakeCodec<MkZC>
where
    MkZC: MakeZeroCopyCodec,
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    type Encoder = DefaultEncoder<MkZC::Encoder, W>;
    type Decoder = DefaultDecoder<MkZC::Decoder, R>;

    fn make_codec(&self, reader: R, writer: W) -> (Self::Encoder, Self::Decoder) {
        let (encoder, decoder) = self.make_zero_copy_codec.make_codec();
        (
            DefaultEncoder {
                encoder,
                writer,
                linked_bytes: LinkedBytes::new(),
            },
            DefaultDecoder {
                decoder,
                reader: BufReader::new(reader),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::DefaultMakeCodec;

    #[test]
    fn test_mk_codec() {
        let _framed = DefaultMakeCodec::framed();
        let _ttheader_framed = DefaultMakeCodec::ttheader_framed();
        let _buffered = DefaultMakeCodec::buffered();
    }
}
