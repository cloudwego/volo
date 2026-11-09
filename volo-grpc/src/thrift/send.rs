//! Bounded producers with synchronous borrowed-message encoding.

use std::{future::Future, marker::PhantomData, sync::Arc};

use futures::{Stream, StreamExt};
use linkedbytes::Node;
use pilota::{
    LinkedBytes,
    thrift::{Message, TLengthProtocol, binary::TBinaryProtocol},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use crate::{
    BoxStream, Request, Status,
    codec::{CodecLimits, Encoder, compression::CompressionEncoding},
    message::CodecOptions,
};

/// Outgoing Thrift messages, either owned values or a bounded encoded channel.
/// A single static dispatch selects the source for the entire HTTP/2 stream.
pub struct SendStream<T>(Source<T>);

enum Source<T> {
    Messages(BoxStream<'static, Result<T, Status>>),
    Prepared(mpsc::Receiver<Result<Prepared, Status>>),
}

impl<T: Message + 'static> SendStream<T> {
    pub fn from_stream(source: impl Stream<Item = Result<T, Status>> + Send + 'static) -> Self {
        Self(Source::Messages(Box::pin(source)))
    }

    #[doc(hidden)]
    pub fn into_body(
        self,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
        classify: Option<fn(&T) -> bool>,
    ) -> BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>> {
        match self.0 {
            Source::Messages(source) => crate::codec::encode::encode_with_context(
                source.map(|value| value.map_err(error_status)),
                compression,
                crate::codec::thrift::ThriftEncoder::default(),
                options.limits,
                options.context,
                classify,
            ),
            Source::Prepared(source) => crate::codec::encode::encode_with_context(
                tokio_stream::wrappers::ReceiverStream::new(source),
                compression,
                PreparedEncoder,
                options.limits,
                options.context,
                None,
            ),
        }
    }
}

/// Channel limits bound queued encoded bytes as well as message count.
#[derive(Clone, Copy, Debug)]
pub struct ChannelConfig {
    pub capacity: usize,
    pub max_buffered_bytes: usize,
    pub max_message_size: usize,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            capacity: 32,
            max_buffered_bytes: 64 * 1024 * 1024,
            max_message_size: CodecLimits::default().max_send_message_size,
        }
    }
}

pub fn channel<T: Message + 'static>(capacity: usize) -> (Sender<T>, SendStream<T>) {
    channel_with_config(ChannelConfig {
        capacity,
        ..ChannelConfig::default()
    })
    .expect("channel capacity must be nonzero")
}

pub fn channel_with_config<T: Message + 'static>(
    config: ChannelConfig,
) -> Result<(Sender<T>, SendStream<T>), Status> {
    if config.capacity == 0
        || config.capacity > Semaphore::MAX_PERMITS
        || config.max_message_size == 0
        || config.max_buffered_bytes < config.max_message_size
        || config.max_buffered_bytes > (u32::MAX as usize).min(Semaphore::MAX_PERMITS)
    {
        return Err(Status::invalid_argument("invalid Thrift channel limits"));
    }
    let (sender, receiver) = mpsc::channel(config.capacity);
    Ok((
        Sender {
            sender,
            bytes: Arc::new(Semaphore::new(config.max_buffered_bytes)),
            max_message_size: config.max_message_size,
            marker: PhantomData,
        },
        SendStream(Source::Prepared(receiver)),
    ))
}

pub struct Sender<T> {
    sender: mpsc::Sender<Result<Prepared, Status>>,
    bytes: Arc<Semaphore>,
    max_message_size: usize,
    marker: PhantomData<fn(T)>,
}

impl<T: Message> Sender<T> {
    /// Encoding completes before this function returns. The future owns only
    /// encoded bytes and sender/permit handles: `&T` never crosses an await and
    /// `T: Sync` is unnecessary. A single non-Clone sender preserves write order;
    /// dropping it half-closes the stream.
    pub fn send<'a>(
        &'a mut self,
        message: &T,
    ) -> impl Future<Output = Result<(), Status>> + Send + use<'a, T> {
        let encoded = if self.sender.is_closed() {
            Err(Status::cancelled("Thrift send stream closed"))
        } else {
            prepare(message, self.max_message_size)
        };
        let sender = self.sender.clone();
        let bytes = self.bytes.clone();
        async move {
            let encoded = encoded?;
            let permit = tokio::select! { biased;
                _ = sender.closed() => return Err(Status::cancelled("Thrift send stream closed")),
                permit = bytes.acquire_many_owned(encoded.len() as u32) => permit.map_err(|_| Status::cancelled("Thrift send stream closed"))?,
            };
            sender
                .send(Ok(Prepared {
                    encoded,
                    _permit: permit,
                }))
                .await
                .map_err(|_| Status::cancelled("Thrift send stream closed"))
        }
    }

    /// Deliver a terminal producer failure without fabricating a message.
    pub fn fail<'a>(
        &'a mut self,
        status: Status,
    ) -> impl Future<Output = Result<(), Status>> + Send + use<'a, T> {
        let sender = self.sender.clone();
        async move {
            if status.code() == crate::Code::Ok {
                return Err(Status::invalid_argument("fail requires a non-OK status"));
            }
            sender
                .send(Err(status))
                .await
                .map_err(|_| Status::cancelled("Thrift send stream closed"))
        }
    }
}

fn error_status(status: Status) -> Status {
    if status.code() == crate::Code::Ok {
        Status::internal("Thrift producer returned OK as an error")
    } else {
        status
    }
}

fn prepare<T: Message>(message: &T, limit: usize) -> Result<LinkedBytes, Status> {
    let mut sizing = TBinaryProtocol::new((), true);
    let size = message.size(&mut sizing);
    if size > limit {
        return Err(Status::resource_exhausted(
            "encoded message exceeds size limit",
        ));
    }
    let mut encoded = LinkedBytes::with_capacity(size.saturating_sub(sizing.zero_copy_len()));
    message
        .encode(&mut TBinaryProtocol::new(&mut encoded, true))
        .map_err(|error| Status::internal(format!("Thrift encoding: {error}")))?;
    if encoded.len() > limit {
        return Err(Status::resource_exhausted(
            "encoded message exceeds size limit",
        ));
    }
    Ok(encoded)
}

struct Prepared {
    encoded: LinkedBytes,
    _permit: OwnedSemaphorePermit,
}
struct PreparedEncoder;
impl Encoder for PreparedEncoder {
    type Item = Prepared;
    type Error = Status;
    fn encode(&mut self, item: Prepared, dst: &mut LinkedBytes) -> Result<(), Status> {
        for node in item.encoded.into_iter_list() {
            match node {
                Node::Bytes(bytes) => dst.insert(bytes),
                Node::BytesMut(bytes) => dst.insert(bytes.freeze()),
                Node::FastStr(value) => dst.insert_faststr(value),
            }
        }
        Ok(())
    }
}

/// Accept a normal Rust stream or the bounded borrowed-send helper without
/// imposing the historical Protobuf `Stream: Sync` bound.
pub trait IntoStreamingRequest {
    type Message: Message + 'static;
    fn into_streaming_request(self) -> Request<SendStream<Self::Message>>;
}
impl<S, T> IntoStreamingRequest for S
where
    S: Stream<Item = T> + Send + 'static,
    T: Message + 'static,
{
    type Message = T;
    fn into_streaming_request(self) -> Request<SendStream<T>> {
        Request::new(SendStream::from_stream(self.map(Ok)))
    }
}
impl<S, T> IntoStreamingRequest for Request<S>
where
    S: Stream<Item = T> + Send + 'static,
    T: Message + 'static,
{
    type Message = T;
    fn into_streaming_request(self) -> Request<SendStream<T>> {
        self.map(|source| SendStream::from_stream(source.map(Ok)))
    }
}
impl<T: Message + 'static> IntoStreamingRequest for SendStream<T> {
    type Message = T;
    fn into_streaming_request(self) -> Request<Self> {
        Request::new(self)
    }
}
impl<T: Message + 'static> IntoStreamingRequest for Request<SendStream<T>> {
    type Message = T;
    fn into_streaming_request(self) -> Self {
        self
    }
}
