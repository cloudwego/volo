#![cfg(feature = "thrift")]

use std::cell::Cell;

use futures::StreamExt;
use pilota::thrift::{ApplicationException, ApplicationExceptionKind, Message, ThriftException};
use volo_grpc::{
    Code,
    body::{Body, boxed},
    codec::{
        decode::Kind,
        thrift::{ThriftDecoder, ThriftRecvStream},
    },
    message::CodecOptions,
    thrift::{ChannelConfig, channel, channel_with_config},
};

// No Clone/Default/Sync bound can be accidentally imposed by the send future.
struct SendOnly {
    message: ApplicationException,
    cell: Cell<u32>,
}

#[tokio::test]
async fn shared_sender_encodes_before_returning_without_clone_or_sync() {
    use volo::streaming::ResponseSender;
    let native = volo_grpc::streaming::StreamContext::new(
        "/test/Call",
        None,
        volo::context::Role::Server,
        None,
    );
    let (sender, stream) = channel::<SendOnly>(1);
    let shared = native.shared_context();
    let mut request =
        volo::streaming::RequestStream::<()>::new(shared.clone(), futures::stream::empty());
    assert!(request.recv().await.unwrap().is_none());
    let mut sender = ResponseSender::new(shared, sender);
    let message = SendOnly {
        message: ApplicationException::new(ApplicationExceptionKind::UNKNOWN, "before"),
        cell: Cell::new(0),
    };
    let sent = sender.send(&message);
    assert_eq!(message.cell.get(), 1);
    message.cell.set(7);
    drop(message);
    fn assert_send<F: Future + Send>(future: F) -> F {
        future
    }
    assert_send(sent).await.unwrap();
    drop(sender);
    let frames = stream.into_body(None, CodecOptions::new(Default::default()), None);
    let mut messages: ThriftRecvStream<SendOnly> = volo_grpc::RecvStream::with_decoder(
        boxed(Body::new(frames)),
        Kind::Request,
        None,
        ThriftDecoder::default(),
    );
    assert_eq!(
        messages.next().await.unwrap().unwrap().message.message(),
        "before"
    );
    assert!(messages.next().await.is_none());
}

#[tokio::test]
async fn shared_sender_cancellation_wakes_a_full_native_queue_while_body_is_owned() {
    use volo::streaming::{ResponseSender, RpcError};
    let native = volo_grpc::streaming::StreamContext::new(
        "/test/Call",
        None,
        volo::context::Role::Server,
        None,
    );
    let shared = native.shared_context();
    let (sender, stream) = channel::<SendOnly>(1);
    let mut sender = ResponseSender::new(shared.clone(), sender);
    let message = SendOnly {
        message: ApplicationException::new(ApplicationExceptionKind::UNKNOWN, "queued"),
        cell: Cell::new(0),
    };
    sender.send(&message).await.unwrap();
    {
        let pending = sender.send(&message);
        tokio::pin!(pending);
        assert!(futures::poll!(&mut pending).is_pending());
        shared.cancel(RpcError::canceled("stop"));
        assert!(pending.await.is_err());
    }
    let encoded = message.cell.get();
    assert!(sender.send(&message).await.is_err());
    assert_eq!(message.cell.get(), encoded);
    drop(stream);
}

#[tokio::test]
async fn success_is_not_accepted_as_a_producer_error_or_reserved_status_metadata() {
    let (mut sender, stream) = channel::<ApplicationException>(1);
    assert_eq!(
        sender
            .fail(volo_grpc::Status::ok(""))
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    drop((sender, stream));
    let source = volo_grpc::thrift::SendStream::<ApplicationException>::from_stream(
        futures::stream::iter([Err(volo_grpc::Status::ok(""))]),
    );
    let mut frames = source.into_body(None, CodecOptions::new(Default::default()), None);
    assert_eq!(
        frames.next().await.unwrap().unwrap_err().code(),
        Code::Internal
    );
    let cx = volo_grpc::streaming::StreamContext::new(
        "/test/Call",
        None,
        volo::context::Role::Server,
        None,
    );
    let mut metadata = volo_grpc::metadata::MetadataMap::new();
    metadata.insert("grpc-status", "0".parse().unwrap());
    assert_eq!(
        cx.response_trailers(metadata).unwrap_err().code(),
        Code::InvalidArgument
    );
}
impl Message for SendOnly {
    fn encode<P: pilota::thrift::TOutputProtocol>(
        &self,
        protocol: &mut P,
    ) -> Result<(), ThriftException> {
        self.cell.set(self.cell.get() + 1);
        self.message.encode(protocol)
    }
    fn decode<P: pilota::thrift::TInputProtocol>(
        protocol: &mut P,
    ) -> Result<Self, ThriftException> {
        Ok(Self {
            message: ApplicationException::decode(protocol)?,
            cell: Cell::new(0),
        })
    }
    async fn decode_async<P: pilota::thrift::TAsyncInputProtocol>(
        protocol: &mut P,
    ) -> Result<Self, ThriftException> {
        Ok(Self {
            message: ApplicationException::decode_async(protocol).await?,
            cell: Cell::new(0),
        })
    }
    fn size<P: pilota::thrift::TLengthProtocol>(&self, protocol: &mut P) -> usize {
        self.message.size(protocol)
    }
}

#[tokio::test]
async fn borrowed_send_encodes_before_returning_and_the_future_is_send() {
    let (mut sender, stream) = channel::<SendOnly>(1);
    let mut message = SendOnly {
        message: ApplicationException::new(ApplicationExceptionKind::UNKNOWN, "before"),
        cell: Cell::new(0),
    };
    let sent = sender.send(&message);
    message.cell.set(7);
    message.message = ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, "after");
    drop(message);
    fn assert_send<F: Future + Send>(future: F) -> F {
        future
    }
    assert_send(sent).await.unwrap();
    drop(sender);
    let frames = stream.into_body(None, CodecOptions::new(Default::default()), None);
    let mut messages: ThriftRecvStream<SendOnly> = volo_grpc::RecvStream::with_decoder(
        boxed(Body::new(frames)),
        Kind::Request,
        None,
        ThriftDecoder::default(),
    );
    assert_eq!(
        messages.next().await.unwrap().unwrap().message.message(),
        "before"
    );
    assert!(messages.next().await.is_none());
}

#[tokio::test]
async fn bounded_channel_unblocks_waiting_senders_when_receiver_is_dropped() {
    let (mut sender, stream) = channel_with_config::<SendOnly>(ChannelConfig {
        capacity: 1,
        max_message_size: 1024,
        max_buffered_bytes: 1024,
        unsafe_codec: false,
    })
    .unwrap();
    let message = SendOnly {
        message: ApplicationException::new(ApplicationExceptionKind::UNKNOWN, "queued"),
        cell: Cell::new(0),
    };
    sender.send(&message).await.unwrap();
    let sent = sender.send(&message);
    tokio::pin!(sent);
    assert!(futures::poll!(&mut sent).is_pending());
    drop(stream);
    assert_eq!(sent.await.unwrap_err().code(), Code::Cancelled);
}
