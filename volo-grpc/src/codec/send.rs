//! One bounded prepared-message queue shared by the payload codecs.

use std::{
    marker::PhantomData,
    sync::Arc,
    task::{Context, Poll, ready},
};

use bytes::{Buf, BufMut};
use futures::{Stream, StreamExt, future::BoxFuture};
use linkedbytes::Node;
use pilota::LinkedBytes;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use super::{CodecLimits, Encoder, compression::CompressionEncoding};
use crate::{BoxStream, Status, message::CodecOptions, streaming::StreamContext};

pub(crate) struct SendStream<T>(Source<T>);
enum Source<T> {
    Messages(BoxStream<'static, Result<T, Status>>),
    Prepared(
        mpsc::Receiver<Result<Prepared, Status>>,
        Option<StreamContext>,
    ),
}
impl<T: Send + 'static> SendStream<T> {
    pub(crate) fn from_stream(
        source: impl Stream<Item = Result<T, Status>> + Send + 'static,
    ) -> Self {
        Self(Source::Messages(Box::pin(source)))
    }
    pub(crate) fn into_body<E>(
        self,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
        encoder: E,
        classify: Option<fn(&T) -> bool>,
    ) -> BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>>
    where
        E: Encoder<Item = T> + Send + 'static,
        E::Error: Into<Status>,
    {
        match self.0 {
            Source::Messages(source) => super::encode::encode_with_context(
                source.map(|value| value.map_err(error_status)),
                compression,
                encoder,
                options.limits,
                options.context,
                classify,
            ),
            Source::Prepared(source, None) => super::encode::encode_with_context(
                tokio_stream::wrappers::ReceiverStream::new(source),
                compression,
                PreparedEncoder,
                options.limits,
                options.context,
                None,
            ),
            Source::Prepared(source, Some(cx)) => super::encode::encode_with_context(
                prepared_stream(source, cx.clone()),
                compression,
                PreparedEncoder,
                options.limits,
                Some(cx),
                None,
            ),
        }
    }
}

/// Per-response encoded queue limits, independent of the HTTP/2 send window.
#[derive(Clone, Copy, Debug)]
pub struct ChannelConfig {
    pub capacity: usize,
    pub max_buffered_bytes: usize,
    pub max_message_size: usize,
    /// Select the optimized Thrift encoder before messages are prepared.
    pub unsafe_codec: bool,
}
impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            capacity: 32,
            unsafe_codec: false,
            max_buffered_bytes: 64 * 1024 * 1024,
            max_message_size: CodecLimits::default().max_send_message_size,
        }
    }
}

impl ChannelConfig {
    /// Preserve both the call's encoding limit and encoder selection.
    pub fn for_options(options: &CodecOptions) -> Self {
        Self {
            unsafe_codec: options.unsafe_codec,
            ..Self::for_limits(options.limits)
        }
    }

    /// Preserve the call's encoding limit while keeping a finite byte budget.
    pub fn for_limits(limits: CodecLimits) -> Self {
        let max_message_size = limits
            .max_send_message_size
            .min((u32::MAX as usize).min(Semaphore::MAX_PERMITS));
        let default = Self::default();
        Self {
            max_message_size,
            max_buffered_bytes: default.max_buffered_bytes.max(max_message_size),
            ..default
        }
    }
}

pub(crate) fn channel<T>(
    config: ChannelConfig,
    prepare: fn(&T, usize) -> Result<LinkedBytes, Status>,
    context: Option<StreamContext>,
) -> Result<(Sender<T>, SendStream<T>), Status> {
    if config.capacity == 0
        || config.capacity > Semaphore::MAX_PERMITS
        || (config.max_message_size == 0 && context.is_none())
        || config.max_buffered_bytes < config.max_message_size
        || config.max_buffered_bytes > (u32::MAX as usize).min(Semaphore::MAX_PERMITS)
    {
        return Err(Status::invalid_argument("invalid channel limits"));
    }
    let (sender, receiver) = mpsc::channel(config.capacity);
    Ok((
        Sender {
            sender,
            bytes: Arc::new(Semaphore::new(config.max_buffered_bytes)),
            max_message_size: config.max_message_size,
            prepare,
            context: context.clone(),
            marker: PhantomData,
            pending: None,
        },
        SendStream(Source::Prepared(receiver, context)),
    ))
}

/// Reserve the envelope in the same allocation as the first encoded fields.
pub(crate) fn prepared_buffer(payload_capacity: usize) -> LinkedBytes {
    let mut bytes = LinkedBytes::with_capacity(super::PREFIX_LEN + payload_capacity);
    bytes.put_slice(&[0; super::PREFIX_LEN]);
    bytes
}

pub struct Sender<T> {
    sender: mpsc::Sender<Result<Prepared, Status>>,
    bytes: Arc<Semaphore>,
    max_message_size: usize,
    prepare: fn(&T, usize) -> Result<LinkedBytes, Status>,
    context: Option<StreamContext>,
    marker: PhantomData<fn(T)>,
    pending: Option<PollSend>,
}
struct PollSend {
    sender: tokio_util::sync::PollSender<Result<Prepared, Status>>,
    bytes: tokio_util::sync::PollSemaphore,
    closed: BoxFuture<'static, ()>,
    encoded: Option<LinkedBytes>,
    permit: Option<OwnedSemaphorePermit>,
    acquiring_bytes: bool,
}
impl<T: 'static> volo::streaming::ResponseSink<T> for Sender<T> {
    fn prepare(&mut self, value: &T) -> volo::streaming::RpcResult<()> {
        // A safely forgotten send future must not lend its old byte permit to
        // a differently sized message on the next mutable borrow.
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.encoded.is_some())
        {
            self.abort_send();
        }
        if self.sender.is_closed() {
            return Err(Status::cancelled("send stream closed").into());
        }
        if let Some(error) = self
            .context
            .as_ref()
            .and_then(StreamContext::producer_error)
        {
            return Err(error.into());
        }
        let encoded = (self.prepare)(value, self.max_message_size)?;
        let pending = self.pending.get_or_insert_with(|| {
            let closed = self.sender.clone();
            PollSend {
                sender: tokio_util::sync::PollSender::new(self.sender.clone()),
                bytes: tokio_util::sync::PollSemaphore::new(self.bytes.clone()),
                closed: Box::pin(async move { closed.closed().await }),
                encoded: None,
                permit: None,
                acquiring_bytes: false,
            }
        });
        pending.encoded = Some(encoded);
        Ok(())
    }
    fn poll_send(&mut self, task: &mut Context<'_>) -> Poll<volo::streaming::RpcResult<()>> {
        let pending = self.pending.as_mut().expect("send was prepared");
        if self.sender.is_closed() || pending.closed.as_mut().poll(task).is_ready() {
            return Poll::Ready(Err(Status::cancelled("send stream closed").into()));
        }
        if pending.permit.is_none() {
            pending.acquiring_bytes = true;
            let len = pending.encoded.as_ref().expect("prepared bytes").len() - super::PREFIX_LEN;
            pending.permit = Some(
                ready!(pending.bytes.poll_acquire_many(task, len as u32))
                    .ok_or_else(|| Status::cancelled("send stream closed"))?,
            );
            pending.acquiring_bytes = false;
        }
        ready!(pending.sender.poll_reserve(task))
            .map_err(|_| Status::cancelled("send stream closed"))?;
        let mut enqueue = || {
            let prepared = Prepared {
                encoded: pending
                    .encoded
                    .take()
                    .expect("send polled after completion"),
                _permit: pending.permit.take().expect("byte permit acquired above"),
            };
            assert!(
                pending.sender.send_item(Ok(prepared)).is_ok(),
                "reserved DATA slot"
            );
        };
        if let Some(cx) = &self.context {
            cx.commit_produced(enqueue)?;
        } else {
            enqueue();
        }
        Poll::Ready(Ok(()))
    }
    fn abort_send(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.sender.abort_send();
            pending.encoded = None;
            pending.permit = None;
            if pending.acquiring_bytes {
                // Drop an incomplete semaphore acquisition, returning any partial
                // reservation. Successful sends retain the reusable waiter.
                pending.bytes = tokio_util::sync::PollSemaphore::new(self.bytes.clone());
                pending.acquiring_bytes = false;
            }
        }
    }
}

impl<T> Sender<T> {
    /// Prepare owned bytes synchronously. The future never retains `&T`.
    #[cfg(any(feature = "thrift", test))]
    pub fn send<'a>(
        &'a mut self,
        message: &T,
    ) -> impl std::future::Future<Output = Result<(), Status>> + Send + use<'a, T> {
        let encoded = if self.sender.is_closed() {
            Err(Status::cancelled("send stream closed"))
        } else if let Some(error) = self
            .context
            .as_ref()
            .and_then(StreamContext::producer_error)
        {
            Err(error)
        } else {
            (self.prepare)(message, self.max_message_size)
        };
        let sender = self.sender.clone();
        let bytes = self.bytes.clone();
        let context = self.context.clone();
        async move {
            let encoded = encoded?;
            let len = (encoded.len() - super::PREFIX_LEN) as u32;
            let enqueue = |slot: mpsc::Permit<'_, Result<Prepared, Status>>, encoded, permit| {
                let commit = || {
                    slot.send(Ok(Prepared {
                        encoded,
                        _permit: permit,
                    }))
                };
                if let Some(cx) = &context {
                    cx.commit_produced(commit)?;
                } else {
                    commit();
                }
                Ok(())
            };
            // No waiter registration when both budgets are available. Commit
            // still checks cancellation after encoding and permit acquisition.
            if let Ok(permit) = bytes.clone().try_acquire_many_owned(len) {
                match sender.try_reserve() {
                    Ok(slot) => return enqueue(slot, encoded, permit),
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Err(Status::cancelled("send stream closed"));
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {}
                }
            }
            let send = async {
                let permit = tokio::select! { biased;
                    _ = sender.closed() => return Err(Status::cancelled("send stream closed")),
                    permit = bytes.acquire_many_owned(len) => permit.map_err(|_| Status::cancelled("send stream closed"))?,
                };
                let slot = sender
                    .reserve()
                    .await
                    .map_err(|_| Status::cancelled("send stream closed"))?;
                enqueue(slot, encoded, permit)
            };
            if let Some(cx) = &context {
                tokio::select! { biased;
                    status = cx.producer_ended() => Err(status),
                    result = send => result,
                }
            } else {
                send.await
            }
        }
    }
    /// Submit an explicit producer failure on a native, non-handler channel.
    #[cfg(feature = "thrift")]
    pub fn fail<'a>(
        &'a mut self,
        status: Status,
    ) -> impl std::future::Future<Output = Result<(), Status>> + Send + use<'a, T> {
        let sender = self.sender.clone();
        async move {
            if status.code() == crate::Code::Ok {
                return Err(Status::invalid_argument("fail requires a non-OK status"));
            }
            sender
                .send(Err(status))
                .await
                .map_err(|_| Status::cancelled("send stream closed"))
        }
    }
}

fn prepared_stream(
    mut receiver: mpsc::Receiver<Result<Prepared, Status>>,
    cx: StreamContext,
) -> impl Stream<Item = Result<Prepared, Status>> + Send + 'static {
    async_stream::stream! {
        loop {
            let changed = cx.production_changed();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(status) = cx.production_result() {
                // The production gate has closed, so every committed enqueue
                // is already visible. Outstanding send futures/Tx owners are
                // not part of the response and must not delay its Trailer.
                receiver.close();
                while let Ok(value) = receiver.try_recv() { yield value; }
                if status.code() != crate::Code::Ok { yield Err(status); }
                break;
            }
            match receiver.try_recv() {
                Ok(value) => yield value,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    if let Some(status) = cx.production_result() {
                        if status.code() != crate::Code::Ok { yield Err(status); }
                        break;
                    }
                    tokio::select! {
                        _ = &mut changed => {},
                        status = cx.cancelled() => { yield Err(status); break; }
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    tokio::select! { biased;
                        status = cx.cancelled() => { yield Err(status); break; },
                        _ = &mut changed => {},
                        value = receiver.recv() => if let Some(value) = value { yield value; },
                    }
                }
            }
        }
    }
}
fn error_status(status: Status) -> Status {
    if status.code() == crate::Code::Ok {
        Status::internal("producer returned OK as an error")
    } else {
        status
    }
}
struct Prepared {
    encoded: LinkedBytes,
    _permit: OwnedSemaphorePermit,
}
struct PreparedEncoder;
impl Encoder for PreparedEncoder {
    type Item = Prepared;
    type Error = Status;
    fn buffer_capacity(&self) -> usize {
        0
    }
    fn encode_framed(&mut self, item: Prepared, max_size: usize) -> Result<LinkedBytes, Status> {
        if item.encoded.len() - super::PREFIX_LEN > max_size {
            return Err(Status::resource_exhausted(
                "encoded message exceeds size limit",
            ));
        }
        Ok(item.encoded)
    }
    fn encode(&mut self, mut item: Prepared, dst: &mut LinkedBytes) -> Result<(), Status> {
        // Compression consumes just the payload; the uncompressed path takes
        // ownership of the complete prepared buffer through encode_framed.
        if let Some(Node::BytesMut(first)) = item.encoded.get_list_mut(0) {
            first.advance(super::PREFIX_LEN);
        } else {
            item.encoded.bytes_mut().advance(super::PREFIX_LEN);
        }
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

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        task::Poll,
        time::Duration,
    };

    use bytes::Bytes;
    use volo::context::Role;

    use super::*;

    struct BytesEncoder;
    impl Encoder for BytesEncoder {
        type Item = Bytes;
        type Error = Status;
        fn encode(&mut self, item: Bytes, dst: &mut LinkedBytes) -> Result<(), Status> {
            dst.insert(item);
            Ok(())
        }
    }
    fn prepare(value: &Bytes, limit: usize) -> Result<LinkedBytes, Status> {
        if value.len() > limit {
            return Err(Status::resource_exhausted("fixture message limit"));
        }
        let mut bytes = prepared_buffer(0);
        bytes.insert(value.clone());
        Ok(bytes)
    }

    struct CountedPayload {
        bytes: Bytes,
        encodes: Arc<AtomicUsize>,
    }

    fn prepare_counted(value: &CountedPayload, limit: usize) -> Result<LinkedBytes, Status> {
        assert!(value.bytes.len() <= limit);
        value.encodes.fetch_add(1, Ordering::Relaxed);
        let mut encoded = prepared_buffer(1);
        encoded.put_u8(b'h');
        let mid = value.bytes.len() / 2;
        encoded.insert(value.bytes.slice(..mid));
        encoded.insert(value.bytes.slice(mid..));
        Ok(encoded)
    }

    struct MustUsePrepared;
    impl Encoder for MustUsePrepared {
        type Item = CountedPayload;
        type Error = Status;

        fn encode(&mut self, _: CountedPayload, _: &mut LinkedBytes) -> Result<(), Status> {
            panic!("the Body must use prepared bytes instead of encoding the message again")
        }
    }

    #[tokio::test]
    async fn inherited_cancellation_allows_repeated_shared_send_errors() {
        let parent = StreamContext::new("/parent/M", None, Role::Server, None);
        let child = parent.sync_scope(|| StreamContext::new("/child/M", None, Role::Client, None));
        let (sender, _source) =
            channel(ChannelConfig::default(), prepare, Some(child.clone())).unwrap();
        let mut sender = volo::streaming::ResponseSender::new(child.shared_context(), sender);
        parent.abort(Status::cancelled("parent canceled"));
        for _ in 0..2 {
            assert!(sender.send(&Bytes::from_static(b"message")).await.is_err());
        }
    }

    #[tokio::test]
    async fn inherited_cancellation_skips_native_encoding() {
        let parent = StreamContext::new("/parent/M", None, Role::Server, None);
        let child = parent.sync_scope(|| StreamContext::new("/child/M", None, Role::Client, None));
        let (mut sender, _source) =
            channel(ChannelConfig::default(), prepare_counted, Some(child)).unwrap();
        let message = CountedPayload {
            bytes: Bytes::from_static(b"message"),
            encodes: Arc::new(AtomicUsize::new(0)),
        };
        parent.abort(Status::cancelled("parent canceled"));
        assert_eq!(
            sender.send(&message).await.unwrap_err().code(),
            crate::Code::Cancelled
        );
        assert_eq!(message.encodes.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn native_send_rechecks_parent_cancellation_before_committing_ready_permits() {
        let parent = StreamContext::new("/parent/M", None, Role::Server, None);
        let child = parent.sync_scope(|| StreamContext::new("/child/M", None, Role::Server, None));
        child
            .start_response_handler(std::future::pending())
            .unwrap();
        let (mut sender, source) =
            channel(ChannelConfig::default(), prepare, Some(child.clone())).unwrap();
        let budget = sender.bytes.clone();
        let available = budget.available_permits();
        let Source::Prepared(mut receiver, _) = source.0 else {
            unreachable!()
        };
        let send = sender.send(&Bytes::from_static(b"encoded before cancellation"));
        parent.abort(Status::cancelled("parent canceled"));
        assert_eq!(send.await.unwrap_err().code(), crate::Code::Cancelled);
        assert_eq!(budget.available_permits(), available);
        assert!(receiver.try_recv().is_err());
        child.finish(Status::cancelled("test complete"));
        child.shared_context().reclaimed().await;
    }

    #[tokio::test]
    async fn native_and_shared_senders_preserve_prepared_segments_without_another_encode() {
        for shared in [false, true] {
            for size in [128, 4096, 65536] {
                let cx = StreamContext::new("/test/stream", None, Role::Server, None);
                let (finish, finished) = tokio::sync::oneshot::channel();
                cx.start_response_handler(async move {
                    finished.await.unwrap();
                    Ok(())
                })
                .unwrap();
                let (mut sender, source) =
                    channel(ChannelConfig::default(), prepare_counted, Some(cx.clone())).unwrap();
                let original = Bytes::from(vec![b'x'; size]);
                let encodes = Arc::new(AtomicUsize::new(0));
                let message = CountedPayload {
                    bytes: original.clone(),
                    encodes: encodes.clone(),
                };
                if shared {
                    let mut sender =
                        volo::streaming::ResponseSender::new(cx.shared_context(), sender);
                    let sent = sender.send(&message);
                    assert_eq!(encodes.load(Ordering::Relaxed), 1);
                    drop(message);
                    sent.await.unwrap();
                } else {
                    let sent = sender.send(&message);
                    assert_eq!(encodes.load(Ordering::Relaxed), 1);
                    drop(message);
                    sent.await.unwrap();
                }
                finish.send(()).unwrap();
                let mut body = source.into_body(
                    None,
                    CodecOptions::new(CodecLimits::default()).with_context(Some(cx.clone())),
                    MustUsePrepared,
                    None,
                );
                let prefix = body.next().await.unwrap().unwrap().into_data().unwrap();
                assert_eq!(prefix[0], 0);
                assert_eq!(
                    u32::from_be_bytes(prefix[1..5].try_into().unwrap()) as usize,
                    size + 1
                );
                assert_eq!(&prefix[5..], b"h", "prefix was sent as a separate frame");
                for expected in [original.slice(..size / 2), original.slice(size / 2..)] {
                    let actual = body.next().await.unwrap().unwrap().into_data().unwrap();
                    assert_eq!(actual, expected);
                    assert_eq!(actual.as_ptr(), expected.as_ptr(), "payload was copied");
                }
                assert!(body.next().await.is_none());
                assert_eq!(encodes.load(Ordering::Relaxed), 1);
                cx.finish(Status::ok(""));
                cx.shared_context().reclaimed().await;
            }
        }
    }

    #[tokio::test]
    async fn forgotten_shared_send_cannot_reuse_an_undersized_byte_permit() {
        let cx = StreamContext::new("/S/M", None, Role::Server, None);
        let (sender, source) = channel(
            ChannelConfig {
                capacity: 1,
                max_buffered_bytes: 16,
                max_message_size: 8,
                unsafe_codec: false,
            },
            prepare,
            None,
        )
        .unwrap();
        let budget = sender.bytes.clone();
        let Source::Prepared(mut receiver, _) = source.0 else {
            unreachable!()
        };
        let mut sender = volo::streaming::ResponseSender::new(cx.shared_context(), sender);
        sender.send(&Bytes::from_static(b"data")).await.unwrap();
        let mut forgotten = Box::pin(sender.send(&Bytes::from_static(b"ab")));
        assert!(futures::poll!(&mut forgotten).is_pending());
        assert_eq!(budget.available_permits(), 10);
        std::mem::forget(forgotten);
        drop(receiver.recv().await.unwrap());
        sender.send(&Bytes::from_static(b"12345678")).await.unwrap();
        assert_eq!(budget.available_permits(), 8);
        drop(receiver.recv().await.unwrap());
        assert_eq!(budget.available_permits(), 16);
    }

    #[tokio::test]
    async fn dropped_shared_sends_return_partial_permits_and_do_not_enqueue_stale_bytes() {
        for byte_wait in [false, true] {
            let cx = StreamContext::new("/S/M", None, Role::Server, None);
            let config = ChannelConfig {
                unsafe_codec: false,
                capacity: if byte_wait { 4 } else { 1 },
                max_buffered_bytes: if byte_wait { 8 } else { 16 },
                max_message_size: 8,
            };
            let (sender, source) = channel(config, prepare, None).unwrap();
            let budget = sender.bytes.clone();
            let Source::Prepared(mut receiver, _) = source.0 else {
                unreachable!()
            };
            let mut sender = volo::streaming::ResponseSender::new(cx.shared_context(), sender);
            sender.send(&Bytes::from_static(b"data")).await.unwrap();
            drop(sender.send(&Bytes::from_static(b"unpolled")));
            assert_eq!(budget.available_permits(), config.max_buffered_bytes - 4);
            {
                let pending = sender.send(&Bytes::from_static(b"canceled"));
                tokio::pin!(pending);
                assert!(futures::poll!(&mut pending).is_pending());
                assert_eq!(budget.available_permits(), if byte_wait { 0 } else { 4 });
            }
            assert_eq!(budget.available_permits(), config.max_buffered_bytes - 4);
            drop(receiver.recv().await.unwrap());
            assert_eq!(budget.available_permits(), config.max_buffered_bytes);
            sender.send(&Bytes::from_static(b"next")).await.unwrap();
            let prepared = receiver.recv().await.unwrap().unwrap();
            let mut payload = Vec::new();
            for node in prepared.encoded.into_iter_list() {
                payload.extend_from_slice(node.as_ref());
            }
            assert_eq!(&payload[super::super::PREFIX_LEN..], b"next");
            drop(prepared._permit);
            assert_eq!(budget.available_permits(), config.max_buffered_bytes);
            assert!(receiver.try_recv().is_err());
        }
    }

    // Exercise both waits independently: a full slot queue with bytes already
    // acquired, and an exhausted byte budget with free slots. The encoded Body
    // is never polled (as under a zero H2 window). Inspect the actual semaphore,
    // not a test counter, and retain the sender after termination.
    #[tokio::test]
    async fn unpolled_body_releases_byte_and_slot_waiters_without_leaking_permits() {
        tokio::time::timeout(Duration::from_secs(3), async {
            for byte_wait in [false, true] {
                for cancel_first in [false, true] {
                    let cx = StreamContext::new("/test/stream", None, Role::Server, None);
                    cx.start_response_handler(std::future::pending()).unwrap();
                    let config = ChannelConfig {
                        unsafe_codec: false,
                        capacity: if byte_wait { 8 } else { 1 },
                        max_buffered_bytes: if byte_wait { 4 } else { 12 },
                        max_message_size: 4,
                    };
                    let (mut sender, source) = channel(config, prepare, Some(cx.clone())).unwrap();
                    let budget = sender.bytes.clone();
                    let value = Bytes::from_static(b"data");
                    sender.send(&value).await.unwrap();
                    let body = source.into_body(
                        None,
                        CodecOptions::new(CodecLimits::default()).with_context(Some(cx.clone())),
                        BytesEncoder,
                        None,
                    );
                    {
                        let pending = sender.send(&value);
                        tokio::pin!(pending);
                        assert!(matches!(futures::poll!(&mut pending), Poll::Pending));
                        assert_eq!(budget.available_permits(), if byte_wait { 0 } else { 4 });
                        if cancel_first {
                            cx.abort(Status::cancelled("peer reset"));
                            assert_eq!(pending.await.unwrap_err().code(), crate::Code::Cancelled);
                            // One queued message is still owned by the Body.
                            assert_eq!(budget.available_permits(), config.max_buffered_bytes - 4);
                            drop(body);
                        } else {
                            drop(body);
                            assert_eq!(pending.await.unwrap_err().code(), crate::Code::Cancelled);
                        }
                    }
                    assert_eq!(budget.available_permits(), config.max_buffered_bytes);
                    cx.abort(Status::cancelled("fixture cleanup"));
                    cx.shared_context().reclaimed().await;
                    assert_eq!(
                        sender.send(&value).await.unwrap_err().code(),
                        crate::Code::Cancelled
                    );
                    assert_eq!(budget.available_permits(), config.max_buffered_bytes);
                }
            }
        })
        .await
        .unwrap();
    }
}
