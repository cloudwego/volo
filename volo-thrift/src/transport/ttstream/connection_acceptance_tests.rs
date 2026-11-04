//! Deterministic acceptance tests; all I/O uses Tokio duplex, no sockets.
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use tokio::io::{AsyncRead, ReadBuf};

use super::{super::frame::read_frame, *};
use crate::streaming::StreamStatus;

fn frame(id: i32, kind: FrameType, method: &'static [u8], payload: &'static [u8]) -> Frame {
    let mut frame = Frame::new(
        id,
        kind,
        Bytes::from_static(method),
        Bytes::from_static(payload),
    );
    if kind == FrameType::Header {
        frame.metadata.insert(Bytes::from_static(b"isn"), "Echo");
    }
    frame
}
fn echo_router() -> StreamRouter {
    let mut router = StreamRouter::new();
    router
        .add("Echo", "chat", |_, mut stream| async move {
            while let Some(bytes) = stream.recv_raw().await? {
                stream.send_raw(bytes).await?;
            }
            Ok(())
        })
        .unwrap();
    router
}
fn pair(
    router: StreamRouter,
    client_hooks: Option<HookChain>,
    server_hooks: Option<HookChain>,
) -> (Connection, Connection) {
    let (client, server) = tokio::io::duplex(4096);
    let (cr, cw) = tokio::io::split(client);
    let (sr, sw) = tokio::io::split(server);
    (
        Connection::start(cr, cw, StreamConfig::default(), client_hooks, None).unwrap(),
        Connection::start(sr, sw, StreamConfig::default(), server_hooks, Some(router)).unwrap(),
    )
}
async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn read(wire: &mut tokio::io::DuplexStream, cfg: &StreamConfig) -> Frame {
    tokio::time::timeout(Duration::from_secs(2), read_frame(wire, cfg))
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
#[derive(Default)]
struct Capture(Mutex<Vec<(i32, StreamStatus)>>);
impl StreamHook for Capture {
    fn on_finish(&self, cx: &StreamContext, status: &StreamStatus) {
        self.0.lock().push((cx.stream_id(), status.clone()));
    }
}
fn hooks(capture: &Arc<Capture>) -> Option<HookChain> {
    Some(HookChain::new([capture.clone() as Arc<dyn StreamHook>]))
}

// Mutating NEXT_ID is isolated in an exact-filtered subprocess. It cannot
// interfere with concurrently running library tests in the parent process.
#[test]
fn process_global_sid_wrap_skip_zero_and_cross_connection_allocation() {
    const CHILD: &str = "VOLO_SID_ACCEPTANCE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "transport::ttstream::connection::acceptance_audit::process_global_sid_wrap_skip_zero_and_cross_connection_allocation", "--nocapture"])
            .env(CHILD,"1").output().unwrap();
        assert!(
            output.status.success(),
            "child stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("SID_ACCEPTANCE_CHILD_EXECUTED"));
        return;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            NEXT_ID.store(i32::MAX - 1, Ordering::Relaxed);
            let (a, sa) = pair(echo_router(), None, None);
            let (b, sb) = pair(echo_router(), None, None);
            let a1 = a.open("Echo", "chat", Metadata::default()).await.unwrap();
            let b1 = b.open("Echo", "chat", Metadata::default()).await.unwrap();
            assert_eq!(a1.context().stream_id(), i32::MAX);
            assert_eq!(b1.context().stream_id(), i32::MIN);
            NEXT_ID.store(-1, Ordering::Relaxed);
            let a2 = a.open("Echo", "chat", Metadata::default()).await.unwrap();
            let b2 = b.open("Echo", "chat", Metadata::default()).await.unwrap();
            assert_eq!(a2.context().stream_id(), 1);
            assert_eq!(b2.context().stream_id(), 2);
            // Local active and quarantined IDs are skipped on the same connection.
            a.shared.quarantine.lock().insert(2);
            NEXT_ID.store(0, Ordering::Relaxed);
            let a3 = a.open("Echo", "chat", Metadata::default()).await.unwrap();
            assert_eq!(a3.context().stream_id(), 3);
            for c in [&a, &b, &sa, &sb] {
                c.close();
            }
            drop((a1, b1, a2, b2, a3));
            println!("SID_ACCEPTANCE_CHILD_EXECUTED");
        });
}

#[tokio::test]
async fn quarantine_limit_closes_boundedly_after_unaffected_sibling_progress() {
    let capture = Arc::new(Capture::default());
    let cfg = StreamConfig {
        max_quarantined_ids: 1,
        ..Default::default()
    };
    let (mut wire, io) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(io);
    let server = Connection::start(
        reader,
        writer,
        cfg.clone(),
        hooks(&capture),
        Some(echo_router()),
    )
    .unwrap();
    let weak = Arc::downgrade(&server.shared);
    for id in [1, 2, 1] {
        wire.write_all(
            &frame(id, FrameType::Header, b"chat", b"")
                .encode(&cfg)
                .unwrap(),
        )
        .await
        .unwrap();
    }
    let rst = read(&mut wire, &cfg).await;
    assert_eq!((rst.stream_id, rst.kind), (1, FrameType::Reset));
    assert_eq!(
        crate::streaming::decode_exception(rst.payload)
            .unwrap()
            .kind()
            .as_i32(),
        7
    );
    // A conflicting stream must not stall an already active sibling.
    wire.write_all(
        &frame(2, FrameType::Data, b"chat", b"sibling")
            .encode(&cfg)
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(read(&mut wire, &cfg).await.kind, FrameType::Header);
    let data = read(&mut wire, &cfg).await;
    assert_eq!(
        (data.stream_id, data.payload),
        (2, Bytes::from_static(b"sibling"))
    );
    wire.write_all(
        &frame(2, FrameType::Header, b"chat", b"")
            .encode(&cfg)
            .unwrap(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), server.closed())
        .await
        .unwrap();
    assert!(server.shared.streams.lock().is_empty());
    assert_eq!(server.shared.quarantine.lock().len(), 2); // threshold+one offending ID, then connection exit
    // The control may flush or be discarded by connection close; neither may hang.
    tokio::time::timeout(Duration::from_secs(2), async {
        while read_frame(&mut wire, &cfg).await.unwrap().is_some() {}
    })
    .await
    .unwrap();
    wait_until(|| capture.0.lock().len() == 2).await;
    for (_, s) in capture.0.lock().iter() {
        assert_eq!(s.resources.queued_bytes, 0);
        assert_eq!(s.resources.send_buffer_bytes, 0);
    }
    drop((server, wire));
    wait_until(|| weak.upgrade().is_none()).await;
}

struct OneByte<R> {
    inner: R,
    bytes: Arc<AtomicUsize>,
}
impl<R: AsyncRead + Unpin> AsyncRead for OneByte<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        dst: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if dst.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut byte = [0];
        let mut buf = ReadBuf::new(&mut byte);
        match Pin::new(&mut self.inner).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(())) => {
                let n = buf.filled().len();
                dst.put_slice(buf.filled());
                self.bytes.fetch_add(n, Ordering::Relaxed);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
#[tokio::test]
async fn one_byte_reads_include_rst_and_late_frames_do_not_reopen_streams() {
    let cfg = StreamConfig::default();
    let capture = Arc::new(Capture::default());
    let ready = Arc::new(Notify::new());
    let mut router = echo_router();
    router
        .add("Echo", "hold", {
            let ready = ready.clone();
            move |_, stream| {
                let ready = ready.clone();
                async move {
                    let _stream = stream;
                    ready.notify_one();
                    futures::future::pending::<Result<(), StreamError>>().await
                }
            }
        })
        .unwrap();
    let (mut wire, io) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(io);
    let bytes = Arc::new(AtomicUsize::new(0));
    let server = Connection::start(
        OneByte {
            inner: r,
            bytes: bytes.clone(),
        },
        w,
        cfg.clone(),
        hooks(&capture),
        Some(router),
    )
    .unwrap();
    let mut sent = 0;
    // Out-of-order frames without an active HEADER are ignored, including RST.
    for kind in [FrameType::Data, FrameType::Trailer, FrameType::Reset] {
        let b = frame(99, kind, b"chat", b"").encode(&cfg).unwrap();
        sent += b.len();
        wire.write_all(&b).await.unwrap();
    }
    for f in [
        frame(1, FrameType::Header, b"chat", b""),
        frame(1, FrameType::Data, b"chat", b"first"),
        frame(1, FrameType::Trailer, b"chat", b""),
        frame(1, FrameType::Data, b"chat", b"must-not-arrive"),
        frame(1, FrameType::Trailer, b"chat", b""),
    ] {
        let b = f.encode(&cfg).unwrap();
        sent += b.len();
        wire.write_all(&b).await.unwrap();
    }
    assert_eq!(read(&mut wire, &cfg).await.kind, FrameType::Header);
    assert_eq!(
        read(&mut wire, &cfg).await.payload,
        Bytes::from_static(b"first")
    );
    assert_eq!(read(&mut wire, &cfg).await.kind, FrameType::Trailer);
    let b = frame(2, FrameType::Header, b"hold", b"")
        .encode(&cfg)
        .unwrap();
    sent += b.len();
    wire.write_all(&b).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), ready.notified())
        .await
        .unwrap();
    for kind in [
        FrameType::Data,
        FrameType::Reset,
        FrameType::Trailer,
        FrameType::Reset,
        FrameType::Data,
    ] {
        let b = frame(2, kind, b"hold", b"").encode(&cfg).unwrap();
        sent += b.len();
        wire.write_all(&b).await.unwrap();
    }
    for f in [
        frame(3, FrameType::Header, b"chat", b""),
        frame(3, FrameType::Trailer, b"chat", b""),
    ] {
        let b = f.encode(&cfg).unwrap();
        sent += b.len();
        wire.write_all(&b).await.unwrap();
    }
    for kind in [FrameType::Header, FrameType::Trailer] {
        let response = read(&mut wire, &cfg).await;
        assert_eq!((response.stream_id, response.kind), (3, kind));
    }
    wait_until(|| capture.0.lock().len() == 3).await;
    assert_eq!(bytes.load(Ordering::Relaxed), sent);
    assert_eq!(
        capture.0.lock().iter().filter(|(sid, _)| *sid == 2).count(),
        1
    );
    for (_, s) in capture.0.lock().iter() {
        assert_eq!(s.resources.queued_bytes, 0);
    }
    server.close();
}

#[tokio::test]
async fn simultaneous_peer_cancellation_finishes_once_and_sibling_stays_usable() {
    let client_capture = Arc::new(Capture::default());
    let server_capture = Arc::new(Capture::default());
    let ready = Arc::new(Notify::new());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut router = echo_router();
    router
        .add("Echo", "cancel", {
            let ready = ready.clone();
            let barrier = barrier.clone();
            move |cx, stream| {
                let ready = ready.clone();
                let barrier = barrier.clone();
                async move {
                    let _stream = stream;
                    ready.notify_one();
                    barrier.wait().await;
                    cx.cancel_local("server canceled at shared barrier");
                    futures::future::pending::<Result<(), StreamError>>().await
                }
            }
        })
        .unwrap();
    let (client, server) = pair(router, hooks(&client_capture), hooks(&server_capture));
    let mut sibling = client
        .open("Echo", "chat", Metadata::default())
        .await
        .unwrap();
    let mut canceled = client
        .open("Echo", "cancel", Metadata::default())
        .await
        .unwrap();
    let sid = canceled.context().stream_id();
    tokio::time::timeout(Duration::from_secs(2), ready.notified())
        .await
        .unwrap();
    barrier.wait().await;
    canceled
        .context()
        .cancel_local("client canceled at shared barrier");
    assert!(
        tokio::time::timeout(Duration::from_secs(2), canceled.recv_raw())
            .await
            .unwrap()
            .is_err()
    );
    canceled
        .context()
        .cancel_local("repeat cancellation must be idempotent");
    drop(canceled);
    sibling
        .send_raw(Bytes::from_static(b"sibling survives"))
        .await
        .unwrap();
    sibling.close_send().await.unwrap();
    assert_eq!(
        sibling.recv_raw().await.unwrap(),
        Some(Bytes::from_static(b"sibling survives"))
    );
    assert!(sibling.recv_raw().await.unwrap().is_none());
    drop(sibling);
    wait_until(|| client_capture.0.lock().len() == 2 && server_capture.0.lock().len() == 2).await;
    for capture in [&client_capture, &server_capture] {
        let events = capture.0.lock();
        assert_eq!(events.iter().filter(|(id, _)| *id == sid).count(), 1);
        for (_, s) in events.iter() {
            assert_eq!(s.resources.queued_bytes, 0);
            assert_eq!(s.resources.send_buffer_bytes, 0);
        }
    }
    wait_until(|| client.active_streams() == 0 && server.active_streams() == 0).await;
    client.close();
    server.close();
}

#[tokio::test]
async fn connection_failure_wakes_all_streams_once_and_releases_driver_ownership() {
    let client_capture = Arc::new(Capture::default());
    let server_capture = Arc::new(Capture::default());
    let mut router = StreamRouter::new();
    router
        .add("Echo", "hold", |_, stream| async move {
            let _stream = stream;
            futures::future::pending::<Result<(), StreamError>>().await
        })
        .unwrap();
    let (client, server) = pair(router, hooks(&client_capture), hooks(&server_capture));
    let weak_client = Arc::downgrade(&client.shared);
    let weak_server = Arc::downgrade(&server.shared);
    let mut streams = Vec::new();
    for _ in 0..4 {
        let mut stream = client
            .open("Echo", "hold", Metadata::default())
            .await
            .unwrap();
        stream
            .send_raw(Bytes::from_static(b"buffered"))
            .await
            .unwrap();
        streams.push(stream);
    }
    wait_until(|| {
        server.active_streams() == 4 && server.shared.recv_bytes.load(Ordering::Relaxed) == 32
    })
    .await;
    server.close();
    server.close();
    for stream in &mut streams {
        let error = tokio::time::timeout(Duration::from_secs(2), stream.recv_raw())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kitex_code(), Some(12013));
    }
    drop(streams);
    wait_until(|| client_capture.0.lock().len() == 4 && server_capture.0.lock().len() == 4).await;
    for capture in [&client_capture, &server_capture] {
        let observed = capture.0.lock();
        let ids: HashSet<_> = observed.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), 4);
        for (_, status) in observed.iter() {
            assert_eq!(status.resources.queued_bytes, 0);
            assert_eq!(status.resources.send_buffer_bytes, 0);
            assert_eq!(status.error.as_ref().unwrap().kitex_code(), Some(12013));
        }
    }
    assert!(client.is_closed());
    assert_eq!(client.active_streams(), 0);
    assert_eq!(server.active_streams(), 0);
    assert_eq!(server.shared.recv_bytes.load(Ordering::Relaxed), 0);
    client.close();
    drop((client, server));
    wait_until(|| weak_client.upgrade().is_none() && weak_server.upgrade().is_none()).await;
}
