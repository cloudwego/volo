#![cfg(feature = "ttstream")]

use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use pilota::thrift::{ApplicationException, ApplicationExceptionKind, Message, ThriftException};
use tokio::net::{TcpListener, TcpStream};
use volo::streaming::{Metadata, RpcError, RpcMode, RpcResult, StreamContext};
use volo_thrift::{
    streaming::{
        HookChain, Metadata as NativeMetadata, StreamConfig, StreamContext as NativeContext,
        StreamError, StreamHook, StreamMode, StreamStatus,
    },
    transport::ttstream::connection::{Connection, StreamRouter},
};

async fn pair(router: StreamRouter, hooks: Option<HookChain>) -> (Connection, Connection) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (client, server) = tokio::join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept(),
    );
    let (read, write) = client.unwrap().into_split();
    let client = Connection::start(read, write, StreamConfig::default(), None, None).unwrap();
    let (read, write) = server.unwrap().0.into_split();
    let server =
        Connection::start(read, write, StreamConfig::default(), hooks, Some(router)).unwrap();
    (client, server)
}

#[derive(Default)]
struct Observation {
    finishes: AtomicUsize,
    status: Mutex<Option<(StreamStatus, volo::streaming::StreamStatus)>>,
}
impl StreamHook for Observation {
    fn on_finish(&self, cx: &NativeContext, status: &StreamStatus) {
        self.finishes.fetch_add(1, Ordering::Relaxed);
        *self.status.lock() = Some((status.clone(), cx.shared_context().status().unwrap()));
    }
}

#[tokio::test]
async fn tcp_shared_handler_keeps_half_close_metadata_and_native_terminal() {
    tokio::time::timeout(Duration::from_secs(3), async {
        struct Unique(Box<u32>);
        let mut router = StreamRouter::new();
        router
            .add_mode(
                "Echo",
                "Reply",
                StreamMode::Bidirectional,
                |native, stream| async move {
                    let cx = native.shared_context();
                    let (mut input, mut output) =
                        stream.into_shared::<ApplicationException, ApplicationException>();
                    let result: RpcResult<()> = cx
                        .scope(async {
                            assert_eq!(cx.service(), "Echo");
                            assert_eq!(cx.method(), "Reply");
                            assert_eq!(cx.mode(), Some(RpcMode::Bidirectional));
                            assert_eq!(cx.metadata().get("x-request").unwrap(), "caller");
                            cx.with_extensions(|extensions| {
                                extensions.insert(Unique(Box::new(17)))
                            });
                            native.with_extensions(|extensions| {
                                assert_eq!(*extensions.get::<Unique>().unwrap().0, 17);
                            });
                            let request = input.recv().await?.unwrap();
                            assert!(input.recv().await?.is_none());
                            assert!(cx.status().is_none(), "request EOF is only a half-close");
                            tokio::task::yield_now().await;
                            assert_eq!(StreamContext::current().unwrap().id(), cx.id());
                            assert_eq!(
                                volo_thrift::streaming::StreamContext::current()
                                    .unwrap()
                                    .stream_id() as u32 as u64,
                                cx.id()
                            );
                            let mut headers = Metadata::default();
                            headers.append("x-header", "first");
                            headers.append("x-header", "last");
                            cx.response_headers(headers)?;
                            let mut trailers = Metadata::default();
                            trailers.insert("x-trailer", "final");
                            cx.response_trailers(trailers)?;
                            output.send(&request).await?;
                            assert!(cx.response_headers(Metadata::default()).is_err());
                            Err(RpcError::Biz {
                                code: 37,
                                message: "business result".into(),
                                extra: [("detail".into(), "value".into())].into(),
                            })
                        })
                        .await;
                    result.map_err(StreamError::from)
                },
            )
            .unwrap();
        let observation = Arc::new(Observation::default());
        let mut hooks = HookChain::default();
        hooks.push(observation.clone());
        let (client, server) = pair(router, Some(hooks)).await;
        let mut metadata = NativeMetadata::default();
        metadata.insert("x-request", "caller");
        let (mut tx, mut rx) = client
            .open_context(
                NativeContext::new(-1, "Echo", "Reply", volo::context::Role::Client),
                metadata,
            )
            .await
            .unwrap()
            .into_bidi::<ApplicationException, ApplicationException>()
            .into_split();
        tx.send(&ApplicationException::new(
            ApplicationExceptionKind::UNKNOWN_METHOD,
            "hello",
        ))
        .await
        .unwrap();
        tx.close_send().await.unwrap();
        assert_eq!(rx.recv().await.unwrap().unwrap().message(), "hello");
        assert!(matches!(
            rx.recv().await,
            Err(StreamError::Biz { code: 37, .. })
        ));
        assert_eq!(rx.header().await.unwrap().get("x-header").unwrap(), "last");
        assert_eq!(rx.trailer().unwrap().get("x-trailer").unwrap(), "final");
        while observation.status.lock().is_none() {
            tokio::task::yield_now().await;
        }
        let (native, shared) = observation.status.lock().clone().unwrap();
        assert_eq!(observation.finishes.load(Ordering::Relaxed), 1);
        assert!(matches!(
            native.error,
            Some(StreamError::Biz { code: 37, .. })
        ));
        assert!(matches!(shared.error, Some(RpcError::Biz { code: 37, .. })));
        assert_eq!(shared.header.get("x-header").unwrap(), "last");
        assert_eq!(shared.trailer.get("biz-status").unwrap(), "37");
        assert_eq!(shared.trailer.get("x-trailer").unwrap(), "final");
        assert_eq!((shared.sent_messages, shared.received_messages), (1, 1));
        client.close();
        server.close();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn tcp_peer_cancel_reclaims_common_tasks_and_keeps_other_streams_usable() {
    struct Owned(Arc<AtomicBool>);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        let dropped = Arc::new(AtomicBool::new(false));
        let (ready, native) = tokio::sync::oneshot::channel();
        let ready = Arc::new(Mutex::new(Some(ready)));
        let mut router = StreamRouter::new();
        router
            .add("Echo", "Wait", {
                let dropped = dropped.clone();
                move |native, stream| {
                    let dropped = dropped.clone();
                    let ready = ready.clone();
                    async move {
                        let cx = native.shared_context();
                        let (mut input, _output) =
                            stream.into_shared::<ApplicationException, ApplicationException>();
                        let result: RpcResult<()> = cx
                            .scope(async {
                                assert!(input.recv().await?.is_some());
                                let owned = Owned(dropped);
                                let (started, running) = tokio::sync::oneshot::channel();
                                let work = cx.spawn_scoped(async move {
                                    let _owned = owned;
                                    assert!(StreamContext::current().is_some());
                                    let _ = started.send(());
                                    futures::future::pending::<()>().await;
                                });
                                running.await.unwrap();
                                ready.lock().take().unwrap().send(native.clone()).unwrap();
                                work.await?;
                                Ok(())
                            })
                            .await;
                        result.map_err(StreamError::from)
                    }
                }
            })
            .unwrap();
        router
            .add("Echo", "Probe", |_, stream| async move {
                let (mut input, mut output) =
                    stream.into_shared::<ApplicationException, ApplicationException>();
                output
                    .send(&input.recv().await.map_err(StreamError::from)?.unwrap())
                    .await
                    .map_err(StreamError::from)
            })
            .unwrap();
        let (client, server) = pair(router, None).await;
        let (mut tx, mut rx) = client
            .open("Echo", "Wait", NativeMetadata::default())
            .await
            .unwrap()
            .into_bidi::<ApplicationException, ApplicationException>()
            .into_split();
        let message =
            ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, "request");
        tx.send(&message).await.unwrap();
        let native = native.await.unwrap();
        tx.context().cancel_local("caller stopped");
        assert!(rx.recv().await.is_err());
        native.tasks_reclaimed().await;
        assert!(dropped.load(Ordering::Acquire));
        assert!(matches!(
            native.shared_context().cancelled().await,
            RpcError::Canceled { code: 12007, .. }
        ));
        let (mut tx, mut rx) = client
            .open("Echo", "Probe", NativeMetadata::default())
            .await
            .unwrap()
            .into_bidi::<ApplicationException, ApplicationException>()
            .into_split();
        tx.send(&message).await.unwrap();
        tx.close_send().await.unwrap();
        assert_eq!(rx.recv().await.unwrap().unwrap().message(), "request");
        assert!(rx.recv().await.unwrap().is_none());
        client.close();
        server.close();
    })
    .await
    .unwrap();
}

// Deliberately has neither Clone nor Sync. The owned send future must not keep
// a reference to this message after the synchronous preparation call returns.
struct SendOnly {
    message: ApplicationException,
    encoded: Cell<usize>,
}
impl Message for SendOnly {
    fn encode<P: pilota::thrift::TOutputProtocol>(
        &self,
        protocol: &mut P,
    ) -> Result<(), ThriftException> {
        self.encoded.set(self.encoded.get() + 1);
        self.message.encode(protocol)
    }
    fn decode<P: pilota::thrift::TInputProtocol>(
        protocol: &mut P,
    ) -> Result<Self, ThriftException> {
        Ok(Self {
            message: ApplicationException::decode(protocol)?,
            encoded: Cell::new(0),
        })
    }
    async fn decode_async<P: pilota::thrift::TAsyncInputProtocol>(
        protocol: &mut P,
    ) -> Result<Self, ThriftException> {
        Ok(Self {
            message: ApplicationException::decode_async(protocol).await?,
            encoded: Cell::new(0),
        })
    }
    fn size<P: pilota::thrift::TLengthProtocol>(&self, protocol: &mut P) -> usize {
        self.message.size(protocol)
    }
}

#[tokio::test]
async fn tcp_response_preparation_releases_a_non_sync_message_before_await() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut router = StreamRouter::new();
        router
            .add("Echo", "SendOnly", |_, stream| async move {
                let (mut input, mut output) =
                    stream.into_shared::<ApplicationException, SendOnly>();
                let message = SendOnly {
                    message: input.recv().await.map_err(StreamError::from)?.unwrap(),
                    encoded: Cell::new(0),
                };
                let sent = output.send(&message);
                assert_eq!(message.encoded.get(), 1);
                message.encoded.set(7);
                drop(message);
                fn assert_send<F: std::future::Future + Send>(future: F) -> F {
                    future
                }
                assert_send(sent).await.map_err(StreamError::from)
            })
            .unwrap();
        let (client, server) = pair(router, None).await;
        let (mut tx, mut rx) = client
            .open("Echo", "SendOnly", NativeMetadata::default())
            .await
            .unwrap()
            .into_bidi::<ApplicationException, ApplicationException>()
            .into_split();
        tx.send(&ApplicationException::new(
            ApplicationExceptionKind::UNKNOWN,
            "before await",
        ))
        .await
        .unwrap();
        tx.close_send().await.unwrap();
        assert_eq!(rx.recv().await.unwrap().unwrap().message(), "before await");
        assert!(rx.recv().await.unwrap().is_none());
        client.close();
        server.close();
    })
    .await
    .unwrap();
}
