use std::{
    fmt,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Buf, BufMut, BytesMut};
use futures::{future, Stream};
use futures_util::ready;
use http::StatusCode;
use http_body::Body;
use prost::Message;
use tracing::{debug, trace};

use super::{DefaultDecoder, BUFFER_SIZE, PREFIX_LEN};
use crate::{codec::Decoder, metadata::MetadataMap, status::Code, Status};

/// Streaming Received Request and Received Response.
///
/// Provides an interface for receiving messages and trailers.
pub struct RecvStream<T> {
    body: hyper::Body,
    decoder: DefaultDecoder<T>,
    trailers: Option<MetadataMap>,
    buf: BytesMut,
    state: State,
    kind: Kind,
}

impl<T> Unpin for RecvStream<T> {}

#[derive(Debug, Clone)]
enum State {
    Header,
    Body(usize),
    Error,
}

#[derive(Debug)]
pub enum Kind {
    Request,
    Response(StatusCode),
}

impl<T> RecvStream<T> {
    pub fn new(body: hyper::Body, kind: Kind) -> Self {
        RecvStream {
            body,
            decoder: DefaultDecoder(PhantomData),
            trailers: None,
            buf: BytesMut::with_capacity(BUFFER_SIZE),
            state: State::Header,
            kind,
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

        future::poll_fn(|cx| Pin::new(&mut self.body).poll_trailers(cx))
            .await
            .map(|t| t.map(MetadataMap::from_headers))
            .map_err(|e| Status::from_error(Box::new(e)))
    }

    fn decode_chunk(&mut self) -> Result<Option<T>, Status> {
        if let State::Header = self.state {
            // data is not enough to decode header, return and keep reading
            if self.buf.remaining() < PREFIX_LEN {
                return Ok(None);
            }

            match self.buf.get_u8() {
                0 => false,
                1 => {
                    trace!("[VOLO] compression not supported yet");
                    return Err(Status::new(
                        Code::Unimplemented,
                        "Compression not supported yet".to_string(),
                    ));
                }
                flag => {
                    trace!("[VOLO] unexpected compression flag");
                    let message = format!(
                        "protocol error: received message with invalid compression flag: {} \
                         (valid flags are 0 and 1), while sending request",
                        flag
                    );
                    // https://grpc.github.io/grpc/core/md_doc_compression.html
                    return Err(Status::new(Code::Internal, message));
                }
            };
            let len = self.buf.get_u32() as usize;
            self.buf.reserve(len);

            self.state = State::Body(len);
        }

        if let State::Body(len) = &self.state {
            // data is not enough to decode body, return and keep reading
            if self.buf.remaining() < *len || self.buf.len() < *len {
                return Ok(None);
            }

            return match DefaultDecoder::<T>::decode(&mut self.decoder, &mut self.buf) {
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
        loop {
            if let State::Error = &self.state {
                return Poll::Ready(None);
            }
            if let Some(item) = self.decode_chunk()? {
                return Poll::Ready(Some(Ok(item)));
            }

            let chunk = match ready!(Pin::new(&mut self.body).poll_data(cx)) {
                Some(Ok(d)) => Some(d),
                Some(Err(e)) => {
                    let _ = std::mem::replace(&mut self.state, State::Error);
                    let err: crate::BoxError = e.into();
                    debug!("[VOLO] decoder inner stream error: {:?}", err);
                    let status = Status::from_error(err);
                    return Poll::Ready(Some(Err(status)));
                }
                None => None,
            };

            if let Some(data) = chunk {
                self.buf.put(data);
            } else if self.buf.has_remaining() {
                trace!("[VOLO] unexpected EOF decoding stream");
                return Poll::Ready(Some(Err(Status::new(
                    Code::Internal,
                    "Unexpected EOF decoding stream.".to_string(),
                ))));
            } else {
                break;
            }
        }

        if let Kind::Response(status) = self.kind {
            match ready!(Pin::new(&mut self.body).poll_trailers(cx)) {
                Ok(trailer) => {
                    if let Err(e) =
                        crate::status::Status::infer_grpc_status(trailer.as_ref(), status)
                    {
                        if let Some(e) = e {
                            return Some(Err(e)).into();
                        } else {
                            return Poll::Ready(None);
                        }
                    } else {
                        self.trailers = trailer.map(MetadataMap::from_headers);
                    }
                }
                Err(e) => {
                    let err: crate::BoxError = e.into();
                    debug!("[VOLO] decoder inner trailers error: {:?}", err);
                    let status = Status::from_error(err);
                    return Some(Err(status)).into();
                }
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
