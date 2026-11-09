use std::{future::pending, time::Duration};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

use super::{super::frame::read_frame, *};
use crate::streaming::{StreamStatus, encode_exception};

struct Exit(Option<oneshot::Sender<()>>);
impl Drop for Exit {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

struct Tail {
    cx: StreamContext,
    exited: oneshot::Receiver<()>,
    resume: oneshot::Sender<bool>,
}

#[derive(Default)]
struct Finishes(AtomicUsize);
impl StreamHook for Finishes {
    fn on_finish(&self, _: &StreamContext, _: &StreamStatus) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn router(error: Option<StreamError>) -> (StreamRouter, mpsc::UnboundedReceiver<Tail>) {
    let (ready, receiver) = mpsc::unbounded_channel();
    let mut router = StreamRouter::new();
    router
        .add("Service", "Call", move |cx, stream| {
            let ready = ready.clone();
            let error = error.clone();
            async move {
                let (exit, exited) = oneshot::channel();
                let _exit = Exit(Some(exit));
                let (resume, continued) = oneshot::channel();
                let (mut tx, _rx) = stream.into_raw_split();
                tx.finish(error.map_or(Ok(()), Err)).await?;
                ready
                    .send(Tail { cx, exited, resume })
                    .unwrap_or_else(|_| panic!("test receiver dropped"));
                assert!(!continued.await.unwrap(), "handler panic after response");
                Ok(())
            }
        })
        .unwrap();
    (router, receiver)
}

async fn tcp_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    (peer, server)
}

async fn start(
    router: StreamRouter,
    config: &StreamConfig,
    finishes: Arc<Finishes>,
) -> (TcpStream, Connection) {
    let (peer, socket) = tcp_pair().await;
    let (reader, writer) = socket.into_split();
    let server = Connection::start(
        reader,
        writer,
        config.clone(),
        Some(HookChain::new([finishes as Arc<dyn StreamHook>])),
        Some(router),
    )
    .unwrap();
    (peer, server)
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn header(peer: &mut TcpStream, config: &StreamConfig, sid: i32) {
    let mut header = Frame::new(
        sid,
        FrameType::Header,
        Bytes::from_static(b"Call"),
        Bytes::new(),
    );
    header.metadata.insert("isn", "Service");
    peer.write_all(&header.encode(config).unwrap())
        .await
        .unwrap();
}

async fn read(peer: &mut TcpStream, config: &StreamConfig) -> Option<Frame> {
    tokio::time::timeout(Duration::from_secs(1), read_frame(peer, config))
        .await
        .unwrap()
        .unwrap()
}

async fn response(peer: &mut TcpStream, config: &StreamConfig, sid: i32) {
    for kind in [FrameType::Header, FrameType::Trailer] {
        let frame = read(peer, config).await.unwrap();
        assert_eq!(frame.kind, kind);
        assert_eq!(frame.stream_id, sid);
    }
}

async fn tail(receiver: &mut mpsc::UnboundedReceiver<Tail>) -> Tail {
    tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn exited(tail: &mut Tail) {
    tokio::time::timeout(Duration::from_secs(1), &mut tail.exited)
        .await
        .unwrap()
        .unwrap();
}

async fn reset(peer: &mut TcpStream, config: &StreamConfig, sid: i32) {
    let payload = encode_exception(
        "Call",
        sid,
        &ApplicationException::new(ApplicationExceptionKind::from_i32(12007), "client cancel"),
    )
    .unwrap();
    let mut frame = Frame::new(sid, FrameType::Reset, Bytes::from_static(b"Call"), payload);
    frame.metadata.insert("ttscp", "caller");
    peer.write_all(&frame.encode(config).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn rst_and_eof_cancel_completed_response_handlers_without_rewriting_terminal() {
    for rst in [false, true] {
        for error in [
            None,
            Some(StreamError::Biz {
                code: 42,
                message: "business".into(),
                extra: Default::default(),
            }),
        ] {
            let config = StreamConfig::default();
            let (router, mut ready) = router(error.clone());
            let finishes = Arc::new(Finishes::default());
            let (mut peer, server) = start(router, &config, finishes.clone()).await;
            header(&mut peer, &config, -17).await;
            let mut tail = tail(&mut ready).await;
            response(&mut peer, &config, -17).await;
            wait_until(|| server.active_streams() == 0).await;
            assert_eq!(server.shared.running_handlers(), 1);
            assert!(tail.cx.cancellation().is_none());
            assert!(matches!(
                tail.exited.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            if rst {
                reset(&mut peer, &config, -17).await;
                // Kitex may send this compatibility Trailer after RST.
                let trailer = Frame::new(
                    -17,
                    FrameType::Trailer,
                    Bytes::from_static(b"Call"),
                    Bytes::new(),
                );
                peer.write_all(&trailer.encode(&config).unwrap())
                    .await
                    .unwrap();
            } else {
                peer.shutdown().await.unwrap();
            }
            exited(&mut tail).await;
            wait_until(|| server.shared.running_handlers() == 0).await;
            let canceled = tail.cx.cancellation().unwrap();
            if rst {
                assert!(
                    matches!(canceled, StreamError::Canceled { code: 12007, cause: CancelCause::Upstream, path, .. } if path == ["caller"])
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), read_frame(&mut peer, &config))
                        .await
                        .is_err()
                );
            } else {
                assert_eq!(canceled.kitex_code(), Some(12013));
            }
            assert_eq!(
                tail.cx.status().unwrap().error.map(|e| e.to_string()),
                error.map(|e| e.to_string())
            );
            assert_eq!(finishes.0.load(Ordering::Relaxed), 1);
            server.close();
        }
    }
}

#[tokio::test]
async fn local_deadline_still_bounds_handler_after_trailer() {
    let config = StreamConfig::default();
    let (router, mut ready) = router(None);
    let finishes = Arc::new(Finishes::default());
    let (mut peer, server) = start(router, &config, finishes.clone()).await;
    header(&mut peer, &config, 1).await;
    let mut tail = tail(&mut ready).await;
    response(&mut peer, &config, 1).await;
    wait_until(|| server.active_streams() == 0).await;
    tail.cx
        .set_deadline(Some(Instant::now() + Duration::from_millis(20)));
    exited(&mut tail).await;
    wait_until(|| server.shared.running_handlers() == 0).await;
    assert!(matches!(
        tail.cx.cancellation(),
        Some(StreamError::Canceled {
            cause: CancelCause::Deadline,
            code: 12015,
            ..
        })
    ));
    assert!(tail.cx.status().unwrap().error.is_none());
    assert_eq!(finishes.0.load(Ordering::Relaxed), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), read_frame(&mut peer, &config))
            .await
            .is_err()
    );
    server.close();
}

#[tokio::test]
async fn graceful_shutdown_waits_for_handler_tail_or_cancels_at_deadline() {
    for expire in [false, true] {
        let config = StreamConfig {
            drain_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let (router, mut ready) = router(None);
        let (mut peer, socket) = tcp_pair().await;
        let (reader, writer) = socket.into_split();
        let (stop, stopped) = oneshot::channel();
        let mut server = tokio::spawn(Connection::serve(
            reader,
            writer,
            config.clone(),
            None,
            router,
            async {
                let _ = stopped.await;
            },
        ));
        header(&mut peer, &config, 1).await;
        let mut tail = tail(&mut ready).await;
        response(&mut peer, &config, 1).await;
        stop.send(()).unwrap();
        // A committed response must not let drain return while its handler is live.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut server)
                .await
                .is_err()
        );
        if expire {
            exited(&mut tail).await;
            assert!(matches!(
                tail.cx.cancellation(),
                Some(StreamError::ServerDraining)
            ));
        } else {
            tail.resume.send(false).unwrap();
            tokio::time::timeout(Duration::from_secs(1), tail.exited)
                .await
                .unwrap()
                .unwrap();
            assert!(tail.cx.cancellation().is_none());
        }
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(tail.cx.status().unwrap().error.is_none());
        assert!(
            read(&mut peer, &config).await.is_none(),
            "no RST after committed Trailer"
        );
    }
}

#[tokio::test]
async fn unfinished_handlers_hold_admission_until_actual_exit() {
    let config = StreamConfig {
        max_streams: 1,
        ..Default::default()
    };
    let (router, mut ready) = router(None);
    let (mut peer, server) = start(router, &config, Arc::default()).await;
    header(&mut peer, &config, 1).await;
    let first = tail(&mut ready).await;
    response(&mut peer, &config, 1).await;
    wait_until(|| server.active_streams() == 0).await;
    header(&mut peer, &config, 2).await;
    let rejected = read(&mut peer, &config).await.unwrap();
    assert_eq!(rejected.kind, FrameType::Reset);
    assert_eq!(rejected.stream_id, 2);
    assert_eq!(
        crate::streaming::decode_exception(rejected.payload)
            .unwrap()
            .kind()
            .as_i32(),
        6
    );
    assert!(matches!(
        ready.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    first.resume.send(false).unwrap();
    tokio::time::timeout(Duration::from_secs(1), first.exited)
        .await
        .unwrap()
        .unwrap();
    wait_until(|| server.shared.running_handlers() == 0).await;
    header(&mut peer, &config, 3).await;
    let mut second = tail(&mut ready).await;
    response(&mut peer, &config, 3).await;
    server.close();
    exited(&mut second).await;
}

#[tokio::test]
async fn reused_sid_cancels_old_handler_without_unregistering_new_generation() {
    let config = StreamConfig::default();
    let (router, mut ready) = router(None);
    let (mut peer, server) = start(router, &config, Arc::default()).await;
    header(&mut peer, &config, -1).await;
    let mut old = tail(&mut ready).await;
    response(&mut peer, &config, -1).await;
    wait_until(|| server.active_streams() == 0).await;
    header(&mut peer, &config, -1).await;
    let mut new = tail(&mut ready).await;
    response(&mut peer, &config, -1).await;
    exited(&mut old).await;
    assert!(old.cx.cancellation().is_some());
    assert!(new.cx.cancellation().is_none());
    wait_until(|| server.shared.running_handlers() == 1 && server.active_streams() == 0).await;
    assert!(server.shared.quarantine.lock().is_empty());
    reset(&mut peer, &config, -1).await;
    exited(&mut new).await;
    assert_eq!(new.cx.cancellation().unwrap().kitex_code(), Some(12007));
    wait_until(|| server.shared.running_handlers() == 0).await;
    server.close();
}

#[tokio::test]
async fn handler_panic_after_response_releases_cancellation_owner() {
    let config = StreamConfig::default();
    let (router, mut ready) = router(None);
    let finishes = Arc::new(Finishes::default());
    let (mut peer, server) = start(router, &config, finishes.clone()).await;
    header(&mut peer, &config, 1).await;
    let tail = tail(&mut ready).await;
    response(&mut peer, &config, 1).await;
    tail.resume.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), tail.exited)
        .await
        .unwrap()
        .unwrap();
    wait_until(|| server.shared.running_handlers() == 0 && server.active_streams() == 0).await;
    assert!(tail.cx.status().unwrap().error.is_none());
    assert_eq!(finishes.0.load(Ordering::Relaxed), 1);
    server.close();
}

#[tokio::test]
async fn unpolled_handler_guard_and_closed_registration_do_not_leak() {
    let config = StreamConfig::default();
    let (router, _) = router(None);
    let (_peer, server) = start(router, &config, Arc::default()).await;
    let cx = StreamContext::new(1, "Service", "Call", Role::Server);
    let guard = HandlerGuard::register(&server.shared, &cx).unwrap();
    let task = tokio::spawn(async move {
        let _guard = guard;
        pending::<()>().await;
    });
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(server.shared.running_handlers(), 0);
    assert!(
        server
            .shared
            .handlers
            .as_ref()
            .unwrap()
            .lock()
            .contexts
            .is_empty()
    );
    server.close();
    assert!(HandlerGuard::register(&server.shared, &cx).is_err());
    assert_eq!(server.shared.running_handlers(), 0);
}
