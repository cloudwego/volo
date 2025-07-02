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
use pilota::pb::Message;
use tracing::{debug, trace};

use super::{BUFFER_SIZE, DefaultDecoder, PREFIX_LEN};
use crate::{
    Status,
    body::BoxBody,
    codec::{
        Decoder,
        compression::{CompressionEncoding, decompress},
    },
    metadata::MetadataMap,
    status::Code,
};

/// Streaming Received Request and Received Response.
///
/// Provides an interface for receiving messages and trailers.
pub struct RecvStream<T> {
    body: BoxBody,
    decoder: DefaultDecoder<T>,
    trailers: Option<MetadataMap>,
    buf: BytesMut,
    state: State,
    kind: Kind,
    compression_encoding: Option<CompressionEncoding>,
    decompress_buf: BytesMut,
}

impl<T> Unpin for RecvStream<T> {}

#[derive(Debug, Clone)]
enum State {
    Header,
    Body(Option<CompressionEncoding>, usize),
    Error,
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
        RecvStream {
            body,
            decoder: DefaultDecoder(PhantomData),
            trailers: None,
            buf: BytesMut::with_capacity(BUFFER_SIZE),
            state: State::Header,
            kind,
            compression_encoding,
            decompress_buf: BytesMut::new(),
        }
    }
}

impl<T: Message + Default> RecvStream<T> {
    /// Get the next message from the stream.
    async fn message(&mut self) -> Result<Option<T>, Status> {
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
            self.buf.reserve(len);

            self.state = State::Body(compression_encoding, len);
        }

        if let State::Body(compression_encoding, len) = &self.state {
            // data is not enough to decode body, return and keep reading
            if self.buf.remaining() < *len || self.buf.len() < *len {
                return Ok(None);
            }
            trace!("[VOLO-GRPC] streaming reading body: {:?}", self.buf);
            let mut buf = self.buf.split_to(*len);
            let decode_result = if let Some(encoding) = compression_encoding {
                self.decompress_buf.clear();
                if let Err(err) = decompress(*encoding, &mut buf, &mut self.decompress_buf) {
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
                DefaultDecoder::<T>::decode(&mut self.decoder, self.decompress_buf.split().freeze())
            } else {
                DefaultDecoder::<T>::decode(&mut self.decoder, buf.freeze())
            };

            return match decode_result {
                Ok(Some(msg)) => {
                    self.state = State::Header;
                    Ok(Some(msg))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(e),
            };
        }

        Ok(None)
    }
}

impl<T: Message + Default> Stream for RecvStream<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let trailer_frame = loop {
            if let State::Error = &self.state {
                return Poll::Ready(None);
            }
            if let Some(item) = self.decode_chunk()? {
                return Poll::Ready(Some(Ok(item)));
            }

            match ready!(Pin::new(&mut self.body).poll_frame(cx)) {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => self.buf.put(data),
                    Err(trailer) => {
                        break Some(trailer);
                    }
                },
                Some(Err(e)) => {
                    let err: crate::BoxError = e.into();
                    let status = Status::from_error(err);
                    if self.kind == Kind::Request && status.code() == Code::Cancelled {
                        return Poll::Ready(None);
                    }
                    debug!("[VOLO] decoder inner stream error: {:?}", status);
                    let _ = std::mem::replace(&mut self.state, State::Error);
                    return Poll::Ready(Some(Err(status)));
                }
                None => {
                    if self.buf.has_remaining() {
                        debug!("[VOLO] unexpected EOF decoding stream");
                        return Poll::Ready(Some(Err(Status::new(
                            Code::Internal,
                            "Unexpected EOF decoding stream.".to_string(),
                        ))));
                    } else {
                        break None;
                    }
                }
            }
        };

        if let Kind::Response(status) = self.kind {
            let trailer = match trailer_frame.map(|frame| frame.into_trailers()) {
                Some(Ok(trailer)) => Some(trailer),
                Some(Err(_frame)) => {
                    // **unreachable** because the `frame` cannot be `Frame::Data` here
                    debug!("[VOLO] unexpected data from stream");
                    return Poll::Ready(Some(Err(Status::new(
                        Code::Internal,
                        "Unexpected data from stream.".to_string(),
                    ))));
                }
                None => None,
            };

            if let Err(e) = Status::infer_grpc_status(trailer.as_ref(), status) {
                return if let Some(e) = e {
                    Some(Err(e)).into()
                } else {
                    Poll::Ready(None)
                };
            } else {
                self.trailers = trailer.map(MetadataMap::from_headers);
            }
        }

        Poll::Ready(None)
    }
}

impl<T> fmt::Debug for RecvStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvStream").finish()
    }
}
