//! The default codec implementation.
//!
//! We use some internal traits such as [`ZeroCopyEncoder`] and [`ZeroCopyDecoder`] to
//! make the implementation more flexible, which is not desired to be used by others, so
//! we don't provide backward compatibility for them.
//!
//! The main entrypoint is [`DefaultMakeCodec`] which receives [`MakeZeroCopyCodec`], and
//! then creates [`DefaultEncoder`] and [`DefaultDecoder`].
//!
//! [`DefaultMakeCodec`] implements [`MakeCodec`] which is used by [`crate::server::Server`] and
//! [`crate::client::Client`].
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
use std::future::Future;

use bytes::Bytes;
use linkedbytes::LinkedBytes;
use pilota::thrift::ThriftException;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, Interest};
use volo::{net::ext::AsyncExt, util::buf_reader::BufReader};

use self::{framed::MakeFramedCodec, thrift::MakeThriftCodec, ttheader::MakeTTHeaderCodec};
use super::{Decoder, Encoder, MakeCodec};
use crate::{EntryMessage, ThriftMessage, context::ThriftContext};

pub mod framed;
pub mod thrift;
pub mod ttheader;

/// Trait for encoding a [`ThriftMessage`] in place.
///
/// [`ZeroCopyEncoder`] tries to encode a message without copying large data taking the advantage
/// of [`LinkedBytes`], which can insert a [`Bytes`] into the middle of a [`bytes::BytesMut`] and
/// uses writev.
///
/// The recommended length threshold to use `LinkedBytes::insert` is 4KB.
pub trait ZeroCopyEncoder: Send + Sync + 'static {
    /// `encode` can rely on the `cx` to get some information such as the protocol detected by
    /// the decoder.
    fn encode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        linked_bytes: &mut LinkedBytes,
        msg: ThriftMessage<Msg>,
    ) -> Result<(), ThriftException>;

    /// `size` should return the exact size of the encoded message, as we will pre-allocate
    /// a buffer for the encoded message.
    ///
    /// To avoid the overhead of calculating the size again in the `encode` method, the
    /// implementation can cache the size in the struct.
    ///
    /// The returned value is (real_size, recommended_malloc_size).
    fn size<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: &ThriftMessage<Msg>,
    ) -> Result<(usize, usize), ThriftException>;
}

/// Trait for decoding a [`ThriftMessage`] in place.
///
/// [`ZeroCopyDecoder`] tries to decode a message without copying large data, so the [`Bytes`] in
/// the `decode` method is not designed to be reused, and the implementation can use
/// `Bytes::split_to` to get a [`Bytes`] and hand it to the user directly.
pub trait ZeroCopyDecoder: Send + Sync + 'static {
    /// If the outer decoder is framed, it can reads all the payload into a [`Bytes`] and
    /// call this function for better performance.
    fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        bytes: &mut Bytes,
    ) -> Result<Option<ThriftMessage<Msg>>, ThriftException>;

    /// The [`DefaultDecoder`] will always call `decode_async`, so the most outer decoder
    /// must implement this function.
    fn decode_async<
        Msg: Send + EntryMessage,
        Cx: ThriftContext,
        R: AsyncRead + Unpin + Send + Sync,
    >(
        &mut self,
        cx: &mut Cx,
        reader: &mut BufReader<R>,
    ) -> impl Future<Output = Result<Option<ThriftMessage<Msg>>, ThriftException>> + Send;
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

impl<E: ZeroCopyEncoder, W: AsyncWrite + AsyncExt + Unpin + Send + Sync + 'static> Encoder
    for DefaultEncoder<E, W>
{
    #[inline]
    async fn encode<Req: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: ThriftMessage<Req>,
    ) -> Result<(), ThriftException> {
        cx.stats_mut().record_encode_start_at();

        // first, we need to get the size of the message
        let (real_size, malloc_size) = self.encoder.size(cx, &msg)?;
        tracing::trace!(
            "[VOLO] codec encode message real size: {}, malloc size: {}",
            real_size,
            malloc_size
        );
        cx.stats_mut().set_write_size(real_size);

        // reset on entry as well, so we always start from a clean buffer even
        // if the previous encode on this (possibly multiplexed, cross-request
        // reused) encoder bailed out early. Some paths (e.g. the `size(..)?`
        // above) can return before reaching the reset after the write, leaving
        // stale nodes behind; resetting on entry guarantees a clean start
        // regardless of how the previous call exited. This is the fix for the
        // multiplex panic in #222 (commit f05a888).
        self.linked_bytes.reset();
        // then we reserve the size of the message in the linked bytes
        self.linked_bytes.reserve(malloc_size);
        // after that, we encode the message into the linked bytes
        let mut write_result: Result<(), ThriftException> = self
            .encoder
            .encode(cx, &mut self.linked_bytes, msg)
            .inspect_err(|_| {
                // record the error time
                cx.stats_mut().record_encode_end_at();
            });
        if write_result.is_ok() {
            cx.stats_mut().record_encode_end_at();
            // encode end is also write start
            cx.stats_mut().record_write_start_at();

            write_result = self
                .linked_bytes
                .write_all_vectored(&mut self.writer)
                .await
                .map_err(Into::into);
        }
        if write_result.is_ok() {
            write_result = self.writer.flush().await.map_err(Into::into);
        }

        // put write end here so we can also record the time of encode error
        cx.stats_mut().record_write_end_at();

        // reset here (rather than only at the start of the next encode) so the
        // zero-copy Bytes/FastStr references inserted for large fields are
        // dropped as soon as the write completes, releasing their memory
        // without waiting for the next request on this connection.
        self.linked_bytes.reset();

        match write_result {
            Ok(()) => Ok(()),
            Err(mut e) => {
                let msg = format!(
                    ", cx: {:?}, encode real size: {}, malloc size: {}",
                    cx.rpc_info(),
                    real_size,
                    malloc_size
                );
                e.append_msg(&msg);
                tracing::warn!("[VOLO] thrift codec encode message error: {}", e);
                Err(e)
            }
        }
        // write_result
    }

    async fn is_closed(&self) -> bool {
        match self
            .writer
            .ready(Interest::READABLE | Interest::WRITABLE)
            .await
        {
            Ok(ready) => ready.is_read_closed() || ready.is_write_closed(),
            Err(e) => {
                tracing::debug!("[VOLO] thrift codec write half ready error: {}", e);
                true
            }
        }
    }

    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
        self.writer.shmipc_helper()
    }
}

pub struct DefaultDecoder<D, R> {
    decoder: D,
    reader: BufReader<R>,
}

impl<D: ZeroCopyDecoder, R: AsyncRead + AsyncExt + Unpin + Send + Sync + 'static> Decoder
    for DefaultDecoder<D, R>
{
    #[inline]
    async fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<ThriftMessage<Msg>>, ThriftException> {
        let buf = match self.reader.fill_buf().await {
            Ok(buf) => buf,
            Err(e) => {
                #[cfg(feature = "shmipc")]
                {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof
                        && self.shmipc_helper().available()
                    {
                        tracing::trace!(
                            "[VOLO] thrift codec decode message EOF (shmipc), rpcinfo: {:?}",
                            cx.rpc_info()
                        );
                        return Ok(None);
                    }
                }
                return Err(e.into());
            }
        };

        if buf.is_empty() {
            tracing::trace!(
                "[VOLO] thrift codec decode message EOF, rpcinfo: {:?}",
                cx.rpc_info()
            );
            return Ok(None);
        }

        let start = std::time::Instant::now();
        cx.stats_mut().record_decode_start_at();
        cx.stats_mut().record_read_start_at();

        tracing::trace!(
            "[VOLO] codec decode message received: {:?}",
            self.reader.buffer()
        );

        // simply call the inner `decode_async`
        let res = self.decoder.decode_async(cx, &mut self.reader).await;

        let end = std::time::Instant::now();
        cx.stats_mut().record_decode_end_at();
        tracing::trace!("[VOLO] thrift codec decode message cost: {:?}", end - start);

        res
    }

    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
        self.reader.shmipc_helper()
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
    R: AsyncRead + AsyncExt + Unpin + Send + Sync + 'static,
    W: AsyncWrite + AsyncExt + Unpin + Send + Sync + 'static,
{
    type Encoder = DefaultEncoder<MkZC::Encoder, W>;
    type Decoder = DefaultDecoder<MkZC::Decoder, R>;

    #[inline]
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
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };

    use bytes::Bytes;
    use tokio::io::{AsyncBufRead, AsyncRead, ReadBuf};
    use volo::context::RpcInfo;

    use super::*;
    use crate::ThriftMessage;

    #[test]
    fn test_mk_codec() {
        let _framed = DefaultMakeCodec::framed();
        let _ttheader_framed = DefaultMakeCodec::ttheader_framed();
        let _buffered = DefaultMakeCodec::buffered();
    }

    struct MockReader {
        eof_behavior: EofBehavior,
        #[cfg(feature = "shmipc")]
        shmipc_stream: Option<volo::net::shmipc::Stream>,
    }

    enum EofBehavior {
        EmptyBuffer,
        UnexpectedEof,
        OtherError,
    }

    impl AsyncRead for MockReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.eof_behavior {
                EofBehavior::EmptyBuffer => Poll::Ready(Ok(())),
                EofBehavior::UnexpectedEof => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected eof",
                ))),
                EofBehavior::OtherError => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "connection reset",
                ))),
            }
        }
    }

    impl AsyncBufRead for MockReader {
        fn poll_fill_buf(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
            match self.eof_behavior {
                EofBehavior::EmptyBuffer => Poll::Ready(Ok(&[])),
                EofBehavior::UnexpectedEof => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected eof",
                ))),
                EofBehavior::OtherError => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "connection reset",
                ))),
            }
        }

        fn consume(self: Pin<&mut Self>, _amt: usize) {}
    }

    impl volo::net::ext::AsyncExt for MockReader {
        async fn ready(&self, _interest: tokio::io::Interest) -> io::Result<tokio::io::Ready> {
            Ok(tokio::io::Ready::READABLE | tokio::io::Ready::WRITABLE)
        }

        #[cfg(feature = "shmipc")]
        fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
            if let Some(stream) = &self.shmipc_stream {
                stream.helper()
            } else {
                volo::net::shmipc::ShmipcHelper::none()
            }
        }
    }

    /// A writer that discards everything written to it, used to drive the
    /// encoder's write path without a real transport.
    struct MockWriter;

    impl AsyncWrite for MockWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_write_vectored(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bufs: &[io::IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(bufs.iter().map(|b| b.len()).sum()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl volo::net::ext::AsyncExt for MockWriter {
        async fn ready(&self, _interest: tokio::io::Interest) -> io::Result<tokio::io::Ready> {
            Ok(tokio::io::Ready::READABLE | tokio::io::Ready::WRITABLE)
        }

        #[cfg(feature = "shmipc")]
        fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
            volo::net::shmipc::ShmipcHelper::none()
        }
    }

    /// A writer that records everything written to it, so a test can inspect
    /// the exact bytes the encoder produced.
    #[derive(Clone, Default)]
    struct RecordingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl RecordingWriter {
        fn contents(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }
    }

    impl AsyncWrite for RecordingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_write_vectored(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bufs: &[io::IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            let mut guard = self.0.lock().unwrap();
            let mut n = 0;
            for b in bufs {
                guard.extend_from_slice(b);
                n += b.len();
            }
            Poll::Ready(Ok(n))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl volo::net::ext::AsyncExt for RecordingWriter {
        async fn ready(&self, _interest: tokio::io::Interest) -> io::Result<tokio::io::Ready> {
            Ok(tokio::io::Ready::READABLE | tokio::io::Ready::WRITABLE)
        }

        #[cfg(feature = "shmipc")]
        fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
            volo::net::shmipc::ShmipcHelper::none()
        }
    }

    /// An [`EntryMessage`] carrying a single large binary field, so that
    /// encoding it takes the zero-copy path (`LinkedBytes::insert`) instead of
    /// copying the payload into the scratch buffer.
    struct BigField(Bytes);

    impl EntryMessage for BigField {
        fn encode<T: pilota::thrift::TOutputProtocol>(
            &self,
            protocol: &mut T,
        ) -> Result<(), ThriftException> {
            // `write_bytes` routes to `insert` when len >= ZERO_COPY_THRESHOLD.
            protocol.write_bytes(self.0.clone())
        }

        fn decode<T: pilota::thrift::TInputProtocol>(
            _protocol: &mut T,
            _msg_ident: &pilota::thrift::TMessageIdentifier,
        ) -> Result<Self, ThriftException> {
            unreachable!("BigField is encode-only in tests")
        }

        async fn decode_async<T: pilota::thrift::TAsyncInputProtocol>(
            _protocol: &mut T,
            _msg_ident: &pilota::thrift::TMessageIdentifier,
        ) -> Result<Self, ThriftException> {
            unreachable!("BigField is encode-only in tests")
        }

        fn size<T: pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {
            // `bytes_len` accounts the payload as zero-copy when it is large
            // enough, matching the `insert` taken during `encode`.
            protocol.bytes_len(self.0.as_ref())
        }
    }

    fn client_cx() -> crate::context::ClientContext {
        crate::context::ClientContext::new(
            1,
            RpcInfo::with_role(volo::context::Role::Client),
            pilota::thrift::TMessageType::Call,
        )
    }

    fn buffered_encoder() -> DefaultEncoder<thrift::ThriftCodec, MockWriter> {
        buffered_encoder_with(MockWriter)
    }

    fn buffered_encoder_with<W>(writer: W) -> DefaultEncoder<thrift::ThriftCodec, W> {
        let (encoder, _decoder) = thrift::MakeThriftCodec::default().make_codec();
        DefaultEncoder {
            encoder,
            writer,
            linked_bytes: LinkedBytes::new(),
        }
    }

    /// Custom owner whose `Drop` flips a shared flag, so we can assert the
    /// external memory backing a `Bytes::from_owner` is released by `encode`.
    struct DropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>, Vec<u8>);

    impl AsRef<[u8]> for DropFlag {
        fn as_ref(&self) -> &[u8] {
            &self.1
        }
    }

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Builds an 8 KiB payload (above the 4 KiB zero-copy threshold) backed by
    /// a [`DropFlag`] owner, returning the payload and the shared drop flag.
    fn tracked_payload() -> (Bytes, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let payload = Bytes::from_owner(DropFlag(dropped.clone(), vec![0x2c_u8; 8 * 1024]));
        (payload, dropped)
    }

    /// Reproduces the failure mode behind the #222 multiplex panic: an encoder
    /// whose `LinkedBytes` still holds stale nodes from a previous encode that
    /// exited early (e.g. via `size(..)?`) without reaching the post-write
    /// reset. The reset on entry must recover from this, so the next encode
    /// neither panics in `LinkedBytes::reset` nor leaks the stale bytes into
    /// the output.
    #[tokio::test]
    async fn test_encode_entry_reset_recovers_stale_buffer() {
        use bytes::BufMut;

        // Baseline: encode a message on a pristine encoder and capture output.
        let mut clean = buffered_encoder_with(RecordingWriter::default());
        let mut cx = client_cx();
        let msg = ThriftMessage::mk_client_msg(&cx, BigField(Bytes::from(vec![0x42_u8; 8 * 1024])));
        clean.encode(&mut cx, msg).await.expect("clean encode ok");
        let expected = clean.writer.contents();

        // Now build an encoder whose buffer is left dirty, mimicking a previous
        // encode that inserted a large field and then bailed out before the
        // post-write reset. The head node is a `Bytes` (via `insert`), which is
        // exactly the shape that made `LinkedBytes::reset` panic in #222.
        let (encoder, _decoder) = thrift::MakeThriftCodec::default().make_codec();
        let mut dirty_bytes = LinkedBytes::new();
        dirty_bytes.bytes_mut().put_slice(b"stale-header");
        dirty_bytes.insert(Bytes::from(vec![0xff_u8; 8 * 1024]));
        assert!(!dirty_bytes.is_empty(), "precondition: buffer is dirty");
        let mut encoder = DefaultEncoder {
            encoder,
            writer: RecordingWriter::default(),
            linked_bytes: dirty_bytes,
        };

        // Encoding the same message must not panic and must produce exactly the
        // same bytes as the clean encoder: the stale content was dropped.
        let mut cx = client_cx();
        let msg = ThriftMessage::mk_client_msg(&cx, BigField(Bytes::from(vec![0x42_u8; 8 * 1024])));
        encoder
            .encode(&mut cx, msg)
            .await
            .expect("encode after dirty buffer ok");
        assert_eq!(
            encoder.writer.contents(),
            expected,
            "entry reset must discard stale buffer content"
        );
    }

    /// End-to-end check of the "notify external owner" use case: a payload
    /// backed by an external buffer via `Bytes::from_owner` is still alive
    /// before `encode`, and its owner is dropped by the time `encode` returns.
    /// The before/after assertions pin the release to the encode step itself
    /// (the reset-after-flush), not to some earlier or unrelated drop.
    #[tokio::test]
    async fn test_encode_drops_external_owner_after_write() {
        let (payload, dropped) = tracked_payload();

        let mut encoder = buffered_encoder();
        let mut cx = client_cx();
        let msg = ThriftMessage::mk_client_msg(&cx, BigField(payload));

        // The payload is still held by `msg`, so the owner must be alive here.
        assert!(
            !dropped.load(std::sync::atomic::Ordering::SeqCst),
            "external owner must still be alive before encode"
        );

        encoder
            .encode(&mut cx, msg)
            .await
            .expect("encode should ok");

        // `encode` consumed `msg` and reset the buffer after flushing, so the
        // last reference is gone and the owner has been dropped.
        assert!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            "external owner should be dropped once the write completes"
        );
    }

    #[tokio::test]
    async fn test_decode_empty_buffer_returns_none() {
        let reader = MockReader {
            eof_behavior: EofBehavior::EmptyBuffer,
            #[cfg(feature = "shmipc")]
            shmipc_stream: None,
        };
        let mut decoder = DefaultDecoder {
            decoder: thrift::MakeThriftCodec::default().make_codec().1,
            reader: BufReader::new(reader),
        };

        let mut cx = crate::context::ClientContext::new(
            1,
            RpcInfo::with_role(volo::context::Role::Client),
            pilota::thrift::TMessageType::Call,
        );

        let result: Result<Option<ThriftMessage<Bytes>>, _> = decoder.decode(&mut cx).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_decode_unexpected_eof_returns_error() {
        let reader = MockReader {
            eof_behavior: EofBehavior::UnexpectedEof,
            #[cfg(feature = "shmipc")]
            shmipc_stream: None,
        };
        let mut decoder = DefaultDecoder {
            decoder: thrift::MakeThriftCodec::default().make_codec().1,
            reader: BufReader::new(reader),
        };

        let mut cx = crate::context::ClientContext::new(
            1,
            RpcInfo::with_role(volo::context::Role::Client),
            pilota::thrift::TMessageType::Call,
        );

        let result: Result<Option<ThriftMessage<Bytes>>, _> = decoder.decode(&mut cx).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unexpected eof"));
    }

    #[cfg(feature = "shmipc")]
    struct ShmipcTestEnv {
        path: std::path::PathBuf,
    }

    #[cfg(feature = "shmipc")]
    impl ShmipcTestEnv {
        async fn new() -> (Self, volo::net::shmipc::Stream) {
            use std::{
                os::unix::net::SocketAddr,
                sync::atomic::{AtomicUsize, Ordering},
            };

            use motore::service::UnaryService;
            use volo::net::shmipc::{
                Listener,
                addr::{Address, ShmipcMakeTransport},
            };

            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);

            let dir = std::env::temp_dir();
            let path = dir.join(format!(
                "volo_shmipc_test_{}_{}.sock",
                std::process::id(),
                id
            ));
            let _ = std::fs::remove_file(&path);

            let addr_val = SocketAddr::from_pathname(&path).expect("failed to create socket addr");
            let addr = Address::from(addr_val);
            let addr_clone = addr.clone();

            tokio::spawn(async move {
                if let Ok(mut listener) = Listener::listen(addr_clone, None).await {
                    while let Ok(_stream) = listener.accept().await {}
                }
            });

            // Give listener time to start
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            let svc = ShmipcMakeTransport::new();
            let stream = svc
                .call(addr)
                .await
                .expect("failed to connect to shmipc listener");

            (Self { path }, stream)
        }
    }

    #[cfg(feature = "shmipc")]
    impl Drop for ShmipcTestEnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[cfg(all(feature = "shmipc", target_os = "linux"))]
    #[tokio::test]
    async fn test_decode_unexpected_eof_returns_none_when_shmipc_available() {
        let (_env, stream) = ShmipcTestEnv::new().await;

        let reader = MockReader {
            eof_behavior: EofBehavior::UnexpectedEof,
            shmipc_stream: Some(stream),
        };

        let mut decoder = DefaultDecoder {
            decoder: thrift::MakeThriftCodec::default().make_codec().1,
            reader: BufReader::new(reader),
        };

        let mut cx = crate::context::ClientContext::new(
            1,
            RpcInfo::with_role(volo::context::Role::Client),
            pilota::thrift::TMessageType::Call,
        );

        let result: Result<Option<ThriftMessage<Bytes>>, _> = decoder.decode(&mut cx).await;

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(all(feature = "shmipc", target_os = "linux"))]
    #[tokio::test]
    async fn test_decode_other_error_returns_error_when_shmipc_available() {
        let (_env, stream) = ShmipcTestEnv::new().await;

        let reader = MockReader {
            eof_behavior: EofBehavior::OtherError,
            shmipc_stream: Some(stream),
        };

        let mut decoder = DefaultDecoder {
            decoder: thrift::MakeThriftCodec::default().make_codec().1,
            reader: BufReader::new(reader),
        };

        let mut cx = crate::context::ClientContext::new(
            1,
            RpcInfo::with_role(volo::context::Role::Client),
            pilota::thrift::TMessageType::Call,
        );

        let result: Result<Option<ThriftMessage<Bytes>>, _> = decoder.decode(&mut cx).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("connection reset"));
    }
}
