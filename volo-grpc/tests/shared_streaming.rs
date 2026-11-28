use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures::stream;
use metainfo::Forward;
use volo::{
    context::Role,
    streaming::{CancelCause, Metadata, RequestStream, RpcError, StreamContext},
};
use volo_grpc::{Code, Status, message::RpcMode, streaming::StreamContext as NativeContext};

fn native_context() -> NativeContext {
    NativeContext::new(
        "/public.Echo/Call",
        Some(RpcMode::Bidirectional),
        Role::Server,
        None,
    )
}

#[tokio::test]
async fn shared_view_uses_the_native_scope_extensions_and_single_terminal() {
    struct Unique(Box<u32>);
    let native = native_context();
    let shared = native.shared_context();
    assert_eq!(shared.id(), native.id() as u64);
    assert_eq!(shared.service(), "Echo");
    assert_eq!(shared.method(), "Call");
    shared.with_extensions(|extensions| extensions.insert(Unique(Box::new(17))));
    native.with_extensions(|extensions| assert_eq!(*extensions.get::<Unique>().unwrap().0, 17));

    shared
        .scope(async {
            metainfo::METAINFO.with(|slot| slot.borrow_mut().set_transient("trace", "shared"));
            tokio::task::yield_now().await;
            assert_eq!(StreamContext::current().unwrap().id(), shared.id());
            assert_eq!(NativeContext::current().unwrap().id(), native.id());
        })
        .await;
    assert_eq!(
        native
            .with_metainfo(|info| info.get_transient("trace"))
            .as_deref(),
        Some("shared")
    );
    assert!(StreamContext::current().is_none());

    shared.cancel(RpcError::Canceled {
        cause: CancelCause::Deadline,
        code: 12014,
        message: "expired".into(),
        path: vec!["upstream".into()],
    });
    assert_eq!(
        native.status().unwrap().status.code(),
        Code::DeadlineExceeded
    );
    native.abort(Status::internal("late failure"));
    assert!(
        matches!(shared.status().unwrap().error, Some(RpcError::GrpcStatus(status)) if status.code == 4 && status.message == "expired")
    );
    assert!(
        matches!(shared.cancelled().await, RpcError::Canceled { cause: CancelCause::Deadline, code: 12014, path, .. } if path == ["upstream"])
    );
}

#[tokio::test]
async fn managed_work_starts_in_scope_and_is_reclaimed_by_the_native_owner() {
    struct Owned(Arc<AtomicBool>);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        let native = native_context();
        let shared = native.shared_context();
        let id = shared.id();
        assert_eq!(
            shared
                .spawn_scoped(async move {
                    tokio::task::yield_now().await;
                    assert_eq!(StreamContext::current().unwrap().id(), id);
                    assert_eq!(NativeContext::current().unwrap().id() as u64, id);
                    42
                })
                .await
                .unwrap(),
            42
        );
        let dropped = Arc::new(AtomicBool::new(false));
        let owned = Owned(dropped.clone());
        let (started, ready) = tokio::sync::oneshot::channel();
        let work = shared.spawn_scoped(async move {
            let _owned = owned;
            let _ = started.send(());
            futures::future::pending::<()>().await;
        });
        ready.await.unwrap();
        native.abort(Status::deadline_exceeded("expired"));
        assert!(matches!(work.await, Err(RpcError::GrpcStatus(status)) if status.code == 4));
        native.children_reclaimed().await;
        assert!(dropped.load(Ordering::Acquire));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn request_eof_is_half_close_and_cancellation_releases_unread_source() {
    let native = native_context();
    let shared = native.shared_context();
    let mut empty = RequestStream::<()>::new(shared.clone(), stream::empty());
    assert!(empty.recv().await.unwrap().is_none());
    assert!(shared.status().is_none());
    let mut headers = Metadata::default();
    headers.append("x-header", "first");
    headers.append("x-header", "second");
    shared.response_headers(headers).unwrap();
    let mut trailers = Metadata::default();
    trailers.insert("x-trailer", "final");
    shared.response_trailers(trailers).unwrap();

    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<(), RpcError>>(1);
    let mut pending = RequestStream::new(
        shared.clone(),
        tokio_stream::wrappers::ReceiverStream::new(receiver),
    );
    let receive = pending.recv();
    tokio::pin!(receive);
    assert!(futures::poll!(&mut receive).is_pending());
    shared.cancel(RpcError::canceled("stop"));
    assert!(receive.await.is_err());
    assert!(
        sender.is_closed(),
        "cancellation must drop the unread native source"
    );
}

#[test]
fn status_roundtrip_preserves_details_duplicates_and_original_peer_code() {
    let mut headers = Status::with_details(Code::Unknown, "失败 %", Bytes::from_static(b"details"))
        .to_header_map()
        .unwrap();
    headers.insert("grpc-status", "20".parse().unwrap());
    headers.append("x-repeat", "first".parse().unwrap());
    headers.append("x-repeat", "second".parse().unwrap());
    let native = Status::from_header_map(&headers).unwrap();
    let shared: RpcError = native.into();
    assert!(std::mem::size_of::<RpcError>() <= 128);
    let RpcError::GrpcStatus(status) = &shared else {
        panic!("expected native gRPC status");
    };
    assert_eq!(status.code, 2);
    assert_eq!(status.raw_code.as_deref(), Some(b"20".as_slice()));
    assert_eq!(
        status
            .metadata
            .iter()
            .filter(|(key, _)| key.as_ref() == b"x-repeat")
            .count(),
        2
    );
    let native = Status::from(shared);
    assert_eq!(native.code(), Code::Unknown);
    assert_eq!(native.raw_grpc_code(), Some(b"20".as_slice()));
    assert_eq!(native.details(), b"details");
    let headers = native.to_header_map().unwrap();
    assert_eq!(headers["grpc-status"], "2");
    assert_eq!(headers.get_all("x-repeat").iter().count(), 2);
}

#[test]
fn business_mapping_is_available_for_both_payloads() {
    let error = RpcError::Biz {
        code: 17,
        message: "失败".into(),
        extra: [("key".into(), "换行\n".into())].into(),
    };
    let status = Status::from(error);
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(status.metadata().get("biz-status").unwrap(), "17");
    assert!(status.metadata().get("biz-message").is_none());
    assert!(status.metadata().get("biz-extra").is_some());
}

#[tokio::test]
async fn global_invocation_keeps_a_send_only_nonclone_reply_on_the_call_stack() {
    use volo::streaming::{InvocationService, ServerMiddleware};
    struct Reply(std::cell::Cell<usize>);
    fn send<T: Send>(_: &T) {}
    ServerMiddleware::scope_optional(
        Some(ServerMiddleware::new(
            Some(InvocationService::default()),
            Vec::new(),
        )),
        async {
            let cx = native_context().shared_context();
            let future = cx.invoke(|_| async {
                tokio::task::yield_now().await;
                Ok(Reply(std::cell::Cell::new(37)))
            });
            send(&future);
            assert_eq!(future.await.unwrap().0.get(), 37);
        },
    )
    .await;
}

#[tokio::test]
async fn a_layer_cannot_turn_a_skipped_typed_handler_into_a_successful_reply() {
    use volo::streaming::{Invocation, InvocationService, RpcResult, ServerMiddleware};
    struct Skip;
    impl volo::Service<StreamContext, Invocation<'_>> for Skip {
        type Response = ();
        type Error = RpcError;
        async fn call(&self, _: &mut StreamContext, _: Invocation<'_>) -> RpcResult<()> {
            Ok(())
        }
    }
    ServerMiddleware::scope_optional(Some(ServerMiddleware::new(Some(InvocationService::new(Skip)), Vec::new())), async {
        let called = std::sync::atomic::AtomicUsize::new(0);
        let cx = native_context().shared_context();
        let result = cx.invoke(|_| {
            called.fetch_add(1, Ordering::Relaxed);
            async { Ok(37) }
        }).await;
        assert!(matches!(result, Err(RpcError::Protocol(message)) if message.contains("without invoking")));
        assert_eq!(called.load(Ordering::Relaxed), 0);
    }).await;
}
