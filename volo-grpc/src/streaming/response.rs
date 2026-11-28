//! Direct push handlers over the native H2 call, codec and bounded queue.

use std::{future::Future, panic::AssertUnwindSafe};

use futures::{FutureExt, Stream, StreamExt};
use pilota::{LinkedBytes, pb::Message};
use volo::streaming::{RequestStream, ResponseSender, RpcResult, StreamContext as SharedContext};

use super::StreamContext;
pub use crate::codec::send::ChannelConfig;
use crate::{
    Response, Status,
    codec::{self, send},
    message::{CodecOptions, SendEntryMessage},
};

/// Wrap the existing typed decoder without a relay task or another queue.
pub fn request<T: 'static>(
    cx: &StreamContext,
    source: impl Stream<Item = Result<T, Status>> + Send + 'static,
) -> RequestStream<T> {
    RequestStream::new(
        cx.shared_context(),
        source.map(|message| message.map_err(Into::into)),
    )
}

/// Decode a value request directly from the native stream. Generated unary and
/// server-streaming handlers do not expose a RequestStream to the application.
/// Keep native cancellation ahead of buffered input without allocating the
/// shared stream's erased decoder and cancellation future.
#[doc(hidden)]
pub async fn request_one<T, D>(
    cx: &StreamContext,
    mut source: crate::RecvStream<T, D>,
    require_end: bool,
) -> Result<T, Status>
where
    D: codec::Decoder<Item = T> + Unpin,
    D::Error: Into<Status>,
{
    tokio::select! { biased;
        status = cx.producer_ended() => Err(status),
        result = async {
            let value = source.message().await?
                .ok_or_else(|| Status::invalid_argument("missing request message"))?;
            if require_end && source.message().await?.is_some() {
                return Err(Status::invalid_argument("multiple unary request messages"));
            }
            Ok(value)
        } => result,
    }
}

/// Prepared Protobuf output for a generated shared handler binding.
pub struct ProtobufStream<T>(send::SendStream<T>);
impl<T: Message + 'static> ProtobufStream<T> {
    pub fn from_stream(source: impl Stream<Item = Result<T, Status>> + Send + 'static) -> Self {
        Self(send::SendStream::from_stream(source))
    }
}
impl<T: Message + 'static> SendEntryMessage for ProtobufStream<T> {
    fn into_body(
        self,
        compression: Option<codec::compression::CompressionEncoding>,
    ) -> crate::BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>> {
        self.into_body_with_options(compression, CodecOptions::new(codec::CodecLimits::legacy()))
    }
    fn into_body_with_options(
        self,
        compression: Option<codec::compression::CompressionEncoding>,
        options: CodecOptions,
    ) -> crate::BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>> {
        self.0.into_body(
            compression,
            options,
            codec::DefaultEncoder::<T>::default(),
            None,
        )
    }
}

/// Start a business handler and return its native response once DATA or its
/// candidate result is ready. Completion still belongs to the H2 driver.
pub async fn protobuf<T, H, F>(
    cx: StreamContext,
    config: ChannelConfig,
    handler: H,
) -> Result<Response<ProtobufStream<T>>, Status>
where
    T: Message + 'static,
    H: FnOnce(SharedContext, ResponseSender<T>) -> F + Send + 'static,
    F: Future<Output = RpcResult<()>> + Send + 'static,
{
    let (sender, source) = send::channel(config, prepare_protobuf::<T>, Some(cx.clone()))?;
    start(&cx, sender, handler)?;
    cx.response_ready().await?;
    Ok(Response::new(ProtobufStream(source)))
}

/// A single-response call awaits its handler directly, with no producer task
/// or encoded queue. Its result is still a candidate until the driver commits.
pub async fn protobuf_unary<T, H, F>(
    cx: StreamContext,
    handler: H,
) -> Result<Response<ProtobufStream<T>>, Status>
where
    T: Message + 'static,
    H: FnOnce(SharedContext) -> F + Send,
    F: Future<Output = RpcResult<T>> + Send,
{
    let result = single_response(&cx, handler).await?;
    Ok(Response::new(ProtobufStream::from_stream(
        futures::stream::iter([result]),
    )))
}

#[cfg(feature = "thrift")]
pub async fn thrift_unary<T, H, F>(
    cx: StreamContext,
    handler: H,
) -> Result<Response<crate::thrift::SendStream<T>>, Status>
where
    T: pilota::thrift::Message + 'static,
    H: FnOnce(SharedContext) -> F + Send,
    F: Future<Output = RpcResult<T>> + Send,
{
    let result = single_response(&cx, handler).await?;
    Ok(Response::new(crate::thrift::SendStream::from_stream(
        futures::stream::iter([result]),
    )))
}

async fn single_response<T, H, F>(
    cx: &StreamContext,
    handler: H,
) -> Result<Result<T, Status>, Status>
where
    T: Send,
    H: FnOnce(SharedContext) -> F + Send,
    F: Future<Output = RpcResult<T>> + Send,
{
    let _owner = cx.own_response_handler()?;
    let shared = cx.shared_context();
    // Keep the concrete decoder/handler future at one allocation boundary;
    // carrying it inline through the scopes duplicates its state in the
    // generated service future. The handler still runs in the caller's task.
    let result = AssertUnwindSafe(shared.scope(Box::pin(async {
        shared.invoke(handler).await.map_err(Status::from)
    })))
    .catch_unwind()
    .await
    .unwrap_or_else(|_| Err(Status::internal("response handler panicked")))
    .map_err(|status| {
        if status.code() == crate::Code::Ok {
            Status::internal("response handler returned OK as an error")
        } else {
            status
        }
    });
    cx.complete_production(
        result
            .as_ref()
            .err()
            .cloned()
            .unwrap_or_else(|| Status::new(crate::Code::Ok, "")),
    );
    Ok(result)
}

#[cfg(feature = "thrift")]
pub async fn thrift<T, H, F>(
    cx: StreamContext,
    config: ChannelConfig,
    handler: H,
) -> Result<Response<crate::thrift::SendStream<T>>, Status>
where
    T: pilota::thrift::Message + 'static,
    H: FnOnce(SharedContext, ResponseSender<T>) -> F + Send + 'static,
    F: Future<Output = RpcResult<()>> + Send + 'static,
{
    let (sender, source) = crate::thrift::send::response_channel(config, cx.clone())?;
    start(&cx, sender, handler)?;
    cx.response_ready().await?;
    Ok(Response::new(source))
}

fn start<T: 'static, H, F>(
    cx: &StreamContext,
    sender: send::Sender<T>,
    handler: H,
) -> Result<(), Status>
where
    H: FnOnce(SharedContext, ResponseSender<T>) -> F + Send + 'static,
    F: Future<Output = RpcResult<()>> + Send + 'static,
{
    let shared = cx.shared_context();
    cx.start_response_handler(async move {
        let output = ResponseSender::new(shared.clone(), sender);
        shared
            .invoke(move |cx| handler(cx, output))
            .await
            .map_err(Status::from)
    })
}

fn prepare_protobuf<T: Message>(value: &T, limit: usize) -> Result<LinkedBytes, Status> {
    let mut lengths = pilota::pb::EncodeLengthContext::default();
    let size = value.encoded_len(&mut lengths);
    if size > limit {
        return Err(Status::resource_exhausted(
            "encoded message exceeds size limit",
        ));
    }
    let mut bytes = send::prepared_buffer(size.saturating_sub(lengths.zero_copy_len));
    value
        .encode(&mut bytes)
        .map_err(|error| Status::internal(format!("Protobuf encoding: {error}")))?;
    if bytes.len() - crate::codec::PREFIX_LEN > limit {
        return Err(Status::resource_exhausted(
            "encoded message exceeds size limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use volo::context::Role;

    use super::*;
    use crate::{
        Code,
        body::{Body, boxed},
        codec::encode::tests::EchoRequest,
        streaming::{CallGuard, lifecycle_body},
    };

    #[tokio::test]
    async fn prepared_protobuf_reuses_prefix_storage_and_preserves_compression() {
        use bytes::{Bytes, BytesMut};
        use codec::compression::{CompressionEncoding, decompress};

        for compression in [
            None,
            Some(CompressionEncoding::Identity),
            #[cfg(feature = "gzip")]
            Some(CompressionEncoding::Gzip(None)),
            #[cfg(feature = "zlib")]
            Some(CompressionEncoding::Zlib(None)),
            #[cfg(feature = "zstd")]
            Some(CompressionEncoding::Zstd(None)),
        ] {
            for size in [0, 128, 4096, 65536] {
                let message = EchoRequest {
                    message: "x".repeat(size).into(),
                };
                let (mut sender, source) = send::channel(
                    ChannelConfig::default(),
                    prepare_protobuf::<EchoRequest>,
                    None,
                )
                .unwrap();
                sender.send(&message).await.unwrap();
                drop(sender);
                let mut body = source.into_body(
                    compression,
                    CodecOptions::new(Default::default()),
                    codec::DefaultEncoder::default(),
                    None,
                );
                let mut frames = Vec::new();
                while let Some(frame) = body.next().await {
                    frames.push(frame.unwrap().into_data().unwrap());
                }
                let compressed =
                    compression.is_some_and(|encoding| encoding != CompressionEncoding::Identity);
                assert_eq!(frames[0][0], u8::from(compressed));
                if !compressed {
                    assert_eq!(frames.len(), if size >= 4096 { 2 } else { 1 });
                    if size != 0 {
                        assert!(
                            frames[0].len() > codec::PREFIX_LEN,
                            "standalone prefix frame"
                        );
                    }
                    if size >= 4096 {
                        assert_eq!(
                            frames[1].as_ptr(),
                            message.message.as_ptr(),
                            "payload was copied"
                        );
                    }
                }
                let wire: Vec<_> = frames
                    .iter()
                    .flat_map(|frame| frame.iter().copied())
                    .collect();
                assert_eq!(
                    u32::from_be_bytes(wire[1..5].try_into().unwrap()) as usize,
                    wire.len() - 5
                );
                let payload = if compressed {
                    let mut decoded = BytesMut::new();
                    decompress(
                        compression.unwrap(),
                        &mut BytesMut::from(&wire[5..]),
                        &mut decoded,
                    )
                    .unwrap();
                    decoded.freeze()
                } else {
                    Bytes::copy_from_slice(&wire[5..])
                };
                assert_eq!(EchoRequest::decode(payload).unwrap(), message);
            }
        }
    }

    fn value_request(body: crate::body::BoxBody) -> crate::RecvStream<EchoRequest> {
        crate::RecvStream::new(body, codec::decode::Kind::Request, None)
    }

    #[tokio::test]
    async fn native_value_request_keeps_cardinality_and_cancellation_precedence() {
        for messages in 0..=2 {
            let bytes = bytes::Bytes::from(vec![0; messages * 5]);
            let cx = StreamContext::new("/Echo/Call", None, Role::Server, None);
            let input = value_request(boxed(http_body_util::Full::new(bytes.clone())));
            let result = request_one(&cx, input, true).await;
            assert_eq!(result.is_ok(), messages == 1);
            if let Err(status) = result {
                assert_eq!(status.code(), Code::InvalidArgument);
            }

            let input = value_request(boxed(http_body_util::Full::new(bytes)));
            cx.abort(Status::deadline_exceeded("expired before decoding"));
            assert_eq!(
                request_one(&cx, input, false).await.unwrap_err().code(),
                Code::DeadlineExceeded
            );
        }
    }

    #[tokio::test]
    async fn native_value_request_cancels_while_waiting_for_unary_eof() {
        let cx = StreamContext::new("/Echo/Call", None, Role::Server, None);
        let source = futures::stream::iter([Ok(http_body::Frame::data(
            bytes::Bytes::from_static(&[0; 5]),
        ))])
        .chain(futures::stream::pending());
        let input = value_request(boxed(Body::new(Box::pin(source))));
        let receiving = request_one(&cx, input, true);
        tokio::pin!(receiving);
        assert!(futures::poll!(&mut receiving).is_pending());
        cx.abort(Status::cancelled("peer reset while waiting for EOF"));
        assert_eq!(receiving.await.unwrap_err().code(), Code::Cancelled);
    }

    #[tokio::test]
    async fn inline_handler_is_not_reclaimed_until_its_future_is_dropped() {
        let cx = StreamContext::new("/Echo/Collect", None, Role::Server, None);
        let (started, wait) = tokio::sync::oneshot::channel();
        let mut response = Box::pin(protobuf_unary::<EchoRequest, _, _>(
            cx.clone(),
            move |shared| async move {
                assert_eq!(SharedContext::current().unwrap().id(), shared.id());
                let _ = started.send(());
                futures::future::pending().await
            },
        ));
        assert!(futures::poll!(response.as_mut()).is_pending());
        wait.await.unwrap();
        cx.abort(Status::cancelled("peer reset"));
        let mut reclaimed = Box::pin(cx.children_reclaimed());
        assert!(futures::poll!(reclaimed.as_mut()).is_pending());
        drop(response);
        reclaimed.await;
        assert_eq!(cx.status().unwrap().status.code(), Code::Cancelled);
    }

    #[tokio::test]
    async fn sender_drop_waits_for_the_handler_and_trailer_poll_does_not_commit_status() {
        let cx = StreamContext::new("/Echo/Call", None, Role::Server, None);
        cx.attach_h2_owner();
        let (dropped, observed) = tokio::sync::oneshot::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        let serve = protobuf::<EchoRequest, _, _>(
            cx.clone(),
            Default::default(),
            move |_, output| async move {
                drop(output);
                let _ = dropped.send(());
                wait.await.unwrap();
                Err(volo::streaming::RpcError::Protocol(
                    "late handler error".into(),
                ))
            },
        );
        tokio::pin!(serve);
        assert!(futures::poll!(&mut serve).is_pending());
        observed.await.unwrap();
        assert!(futures::poll!(&mut serve).is_pending());
        assert!(cx.status().is_none());
        release.send(()).unwrap();
        let response = serve.await.unwrap();
        let frames = response.into_inner().into_body_with_options(
            None,
            CodecOptions::new(Default::default()).with_context(Some(cx.clone())),
        );
        let mut body =
            lifecycle_body::response(boxed(Body::new(frames)), CallGuard::new(cx.clone()));
        let frame = body.frame().await.unwrap().unwrap();
        assert_ne!(frame.trailers_ref().unwrap()["grpc-status"], "0");
        assert!(
            cx.status().is_none(),
            "a candidate Trailer is not a wire commit"
        );
        assert!(
            cx.trailers().is_none(),
            "do not observe an unsent Trailer as committed"
        );
        drop(body);
        assert_eq!(cx.status().unwrap().status.code(), Code::Cancelled);
    }

    #[tokio::test]
    async fn handler_result_closes_production_even_if_an_unmanaged_task_holds_the_sender() {
        let cx = StreamContext::new("/Echo/Call", None, Role::Server, None);
        let (sender, held) = tokio::sync::oneshot::channel();
        let response = protobuf::<EchoRequest, _, _>(
            cx.clone(),
            Default::default(),
            move |_, output| async move {
                let _ = sender.send(output);
                Ok(())
            },
        )
        .await
        .unwrap();
        let mut held = held.await.unwrap();
        assert!(
            held.send(&EchoRequest {
                message: "late".into()
            })
            .await
            .is_err()
        );
        let frames = response.into_inner().into_body_with_options(
            None,
            CodecOptions::new(Default::default()).with_context(Some(cx.clone())),
        );
        let mut body = Body::new(frames);
        assert_eq!(
            body.frame().await.unwrap().unwrap().trailers_ref().unwrap()["grpc-status"],
            "0"
        );
        assert!(cx.status().is_none());
        cx.finish(Status::new(Code::Ok, ""));
    }
}
