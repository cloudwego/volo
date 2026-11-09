use std::{
    fmt,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, BufMut, BytesMut};
use futures::{Stream, future};
use futures_util::ready;
use http::StatusCode;
use http_body::Body;
use tracing::{debug, trace};

use super::{BUFFER_SIZE, CodecLimits, DefaultDecoder, PREFIX_LEN};
use crate::{
    Status,
    body::BoxBody,
    codec::{
        Decoder,
        compression::{CompressionEncoding, decompress_with_limit},
    },
    metadata::MetadataMap,
    status::Code,
};

/// Streaming Received Request and Received Response.
///
/// Provides an interface for receiving messages and trailers.
pub struct RecvStream<T, D = DefaultDecoder<T>> {
    body: BoxBody,
    decoder: D,
    _marker: PhantomData<fn() -> T>,
    trailers: Option<MetadataMap>,
    buf: BytesMut,
    state: State,
    kind: Kind,
    compression_encoding: Option<CompressionEncoding>,
    decompress_buf: BytesMut,
    limits: CodecLimits,
    context: Option<crate::streaming::CallGuard>,
}

impl<T, D: Unpin> Unpin for RecvStream<T, D> {}

#[derive(Debug, Clone)]
enum State {
    Header,
    Body(Option<CompressionEncoding>, usize),
    Finished,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Request,
    Response(StatusCode),
}

impl<T> RecvStream<T> {
    pub fn new(
        body: BoxBody,
        kind: Kind,
        compression_encoding: Option<CompressionEncoding>,
    ) -> Self {
        Self::with_decoder(body, kind, compression_encoding, DefaultDecoder::default())
    }
}

impl<T, D> RecvStream<T, D> {
    /// Construct a stream with a custom, per-stream decoder.
    ///
    /// Uses the legacy size range; new protocols should set explicit limits
    /// with [`Self::with_limits`]. No Protobuf or `Default` bound is imposed.
    pub fn with_decoder(
        body: BoxBody,
        kind: Kind,
        compression_encoding: Option<CompressionEncoding>,
        decoder: D,
    ) -> Self {
        RecvStream {
            body,
            decoder,
            _marker: PhantomData,
            trailers: None,
            buf: BytesMut::with_capacity(BUFFER_SIZE),
            state: State::Header,
            kind,
            compression_encoding,
            decompress_buf: BytesMut::new(),
            limits: CodecLimits::legacy(),
            context: None,
        }
    }

    pub fn with_limits(mut self, limits: CodecLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Attach the RPC authority explicitly. A response decoder owns successful
    /// completion after its framing and final-status checks; request EOF only
    /// closes the receive direction.
    pub fn with_context(mut self, context: Option<crate::streaming::StreamContext>) -> Self {
        self.context = context.map(|context| {
            let mut guard = crate::streaming::CallGuard::new(context);
            if matches!(self.kind, Kind::Response(_)) {
                guard.context.attach_response_decoder();
            } else {
                guard.disarm();
            }
            guard
        });
        self
    }

    pub fn stream_context(&self) -> Option<&crate::streaming::StreamContext> {
        self.context.as_ref().map(|guard| &guard.context)
    }

    fn fail(&mut self, status: Status) -> Status {
        if let Some(context) = self.stream_context() {
            context.finish(status.clone());
        }
        self.finish_receiving();
        status
    }

    fn finish_receiving(&mut self) {
        self.state = State::Finished;
        // Release/reset unread IO even if the application retains the failed
        // RecvStream. Also release a large message buffer at terminal state.
        self.body = crate::body::empty_body();
        self.buf = BytesMut::new();
        self.decompress_buf = BytesMut::new();
    }
}

impl<T, D> RecvStream<T, D>
where
    D: Decoder<Item = T> + Unpin,
    D::Error: Into<Status>,
{
    /// Get the next message from the stream.
    pub async fn message(&mut self) -> Result<Option<T>, Status> {
        match future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await {
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }

    /// Get the trailers from the stream.
    pub async fn trailers(&mut self) -> Result<Option<MetadataMap>, Status> {
        if let Some(trailers) = self.trailers.take() {
            return Ok(Some(trailers));
        }

        // Ensure read body to the end in case of memory leak.
        // Related issue: https://github.com/hyperium/h2/issues/631.
        while self.message().await?.is_some() {}

        if let Some(trailers) = self.trailers.take() {
            return Ok(Some(trailers));
        }

        let maybe_trailer = future::poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;

        match maybe_trailer {
            Some(Ok(frame)) => match frame.into_trailers() {
                Ok(headers) => Ok(Some(MetadataMap::from_headers(headers))),
                Err(_frame) => {
                    // **unreachable** because the `frame` cannot be `Frame::Data` here
                    debug!("[VOLO] unexpected data from stream");
                    Err(Status::new(
                        Code::Internal,
                        "Unexpected data from stream.".to_string(),
                    ))
                }
            },
            Some(Err(err)) => Err(Status::from_error(Box::new(err))),
            None => Ok(None),
        }
    }

    #[allow(clippy::result_large_err)]
    fn decode_chunk(&mut self) -> Result<Option<T>, Status> {
        if let State::Header = self.state {
            // data is not enough to decode header, return and keep reading
            if self.buf.remaining() < PREFIX_LEN {
                return Ok(None);
            }
            trace!("[VOLO-GRPC] streaming received buf: {:?}", self.buf);

            let compression_encoding = match self.buf.get_u8() {
                0 => None,
                1 => {
                    if self.compression_encoding.is_some() {
                        self.compression_encoding
                    } else {
                        return Err(Status::new(
                            Code::Internal,
                            "protocol error: received message with compressed-flag but no \
                             grpc-encoding was specified"
                                .to_string(),
                        ));
                    }
                }
                flag => {
                    let message = format!(
                        "protocol error: received message with invalid compression flag: {flag} \
                         (valid flags are 0 and 1), while sending request"
                    );
                    // https://grpc.github.io/grpc/core/md_doc_compression.html
                    return Err(Status::new(Code::Internal, message));
                }
            };
            let len = self.buf.get_u32() as usize;
            if len > self.limits.max_receive_message_size {
                return Err(Status::resource_exhausted(
                    "received message exceeds wire size limit",
                ));
            }

            self.state = State::Body(compression_encoding, len);
        }

        if let State::Body(compression_encoding, len) = &self.state {
            if self.buf.len() < *len {
                return Ok(None);
            }
            trace!("[VOLO-GRPC] streaming reading body: {:?}", self.buf);
            let detach_small_payload = *len < 4 * 1024 && self.buf.capacity() > 64 * 1024;
            let mut buf = self.buf.split_to(*len);
            let (decode_result, decoded_size) = if let Some(encoding) = compression_encoding {
                self.decompress_buf.clear();
                if let Err(err) = decompress_with_limit(
                    *encoding,
                    &mut buf,
                    &mut self.decompress_buf,
                    self.limits.max_decompressed_message_size,
                ) {
                    if err.kind() == std::io::ErrorKind::FileTooLarge {
                        return Err(Status::resource_exhausted(
                            "decompressed message exceeds size limit",
                        ));
                    }
                    let message = if let Kind::Response(status) = self.kind {
                        format!(
                            "Error decompressing: {err}, while receiving response with status: \
                             {status}"
                        )
                    } else {
                        format!("Error decompressing: {err}, while sending request")
                    };
                    return Err(Status::new(Code::Internal, message));
                }
                let payload = if self.decompress_buf.len() < 4 * 1024
                    && self.decompress_buf.capacity() > 64 * 1024
                {
                    let payload = bytes::Bytes::copy_from_slice(&self.decompress_buf);
                    self.decompress_buf.clear();
                    payload
                } else {
                    self.decompress_buf.split().freeze()
                };
                let size = payload.len();
                (<D as Decoder>::decode(&mut self.decoder, payload), size)
            } else {
                if buf.len() > self.limits.max_decompressed_message_size {
                    return Err(Status::resource_exhausted(
                        "decoded message exceeds size limit",
                    ));
                }
                let payload = if detach_small_payload {
                    bytes::Bytes::copy_from_slice(&buf)
                } else {
                    buf.freeze()
                };
                let size = payload.len();
                (<D as Decoder>::decode(&mut self.decoder, payload), size)
            };

            self.state = State::Header;
            let value = decode_result.map_err(Into::into)?;
            if value.is_some() {
                if let Some(context) = self.stream_context() {
                    context.check_recv_cardinality()?;
                    context.on_recv(decoded_size);
                }
            }
            return Ok(value);
        }

        Ok(None)
    }
}

impl<T, D> Stream for RecvStream<T, D>
where
    D: Decoder<Item = T> + Unpin,
    D::Error: Into<Status>,
{
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let trailer_frame = loop {
            if let State::Finished = &self.state {
                return Poll::Ready(None);
            }
            let buffered_before = self.buf.len();
            match self.decode_chunk() {
                Ok(Some(item)) => return Poll::Ready(Some(Ok(item))),
                Err(error) => {
                    return Poll::Ready(Some(Err(self.fail(error))));
                }
                Ok(None) => {
                    // A custom decoder may skip a whole message. Continue with
                    // already buffered messages instead of waiting for more IO.
                    if self.buf.len() < buffered_before
                        && matches!(self.state, State::Header)
                        && self.buf.len() >= PREFIX_LEN
                    {
                        continue;
                    }
                }
            }

            match ready!(Pin::new(&mut self.body).poll_frame(cx)) {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => {
                        if let State::Body(_, len) = self.state {
                            let buffered = self.buf.len();
                            // Count only received DATA. Reserve before appending
                            // the frame that crosses the threshold so it cannot
                            // grow the buffer once here and again on decode.
                            if self.buf.capacity() < len
                                && buffered.saturating_add(data.len()) >= len.div_ceil(4)
                            {
                                self.buf.reserve(len - buffered);
                            }
                        }
                        self.buf.put(data);
                    }
                    Err(trailer) => {
                        break Some(trailer);
                    }
                },
                Some(Err(e)) => {
                    let err: crate::BoxError = e.into();
                    let status = Status::from_error(err);
                    debug!("[VOLO] decoder inner stream error: {:?}", status);
                    return Poll::Ready(Some(Err(self.fail(status))));
                }
                None => {
                    if self.buf.has_remaining() || matches!(self.state, State::Body(..)) {
                        debug!("[VOLO] unexpected EOF decoding stream");
                        let status = Status::new(
                            Code::Internal,
                            "Unexpected EOF decoding stream.".to_string(),
                        );
                        return Poll::Ready(Some(Err(self.fail(status))));
                    } else {
                        break None;
                    }
                }
            }
        };

        let partial_message = self.buf.has_remaining() || matches!(self.state, State::Body(..));
        let trailer = match trailer_frame.map(|frame| frame.into_trailers()) {
            Some(Ok(trailer)) => Some(trailer),
            Some(Err(_frame)) => {
                // **unreachable** because the `frame` cannot be `Frame::Data` here
                debug!("[VOLO] unexpected data from stream");
                let status =
                    Status::new(Code::Internal, "Unexpected data from stream.".to_string());
                return Poll::Ready(Some(Err(self.fail(status))));
            }
            None => None,
        };

        let result = if let Kind::Response(status) = self.kind {
            Status::infer_grpc_status(trailer.as_ref(), status)
        } else {
            Ok(())
        };
        self.trailers = trailer.map(MetadataMap::from_headers);
        if let Err(error) = result {
            let error =
                error.unwrap_or_else(|| Status::internal("response ended without grpc-status"));
            return Poll::Ready(Some(Err(self.fail(error))));
        }

        if partial_message {
            return Poll::Ready(Some(Err(
                self.fail(Status::internal("unexpected EOF decoding stream"))
            )));
        }

        if matches!(self.kind, Kind::Response(_)) {
            if let Some(context) = self.stream_context() {
                if let Err(status) = context.validate_success_cardinality() {
                    return Poll::Ready(Some(Err(self.fail(status))));
                }
                context.finish(Status::new(Code::Ok, ""));
            }
        }
        self.finish_receiving();

        Poll::Ready(None)
    }
}

impl<T, D> futures::stream::FusedStream for RecvStream<T, D>
where
    D: Decoder<Item = T> + Unpin,
    D::Error: Into<Status>,
{
    fn is_terminated(&self) -> bool {
        matches!(self.state, State::Finished)
    }
}

impl<T, D> fmt::Debug for RecvStream<T, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvStream").finish()
    }
}
