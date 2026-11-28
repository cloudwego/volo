use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::Instant,
};

use bytes::Bytes;
use futures::StreamExt;
use metainfo::Forward;
use volo::context::{MetaInfoScope, Role};

struct CountingAllocator;
thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

fn count() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}

// SAFETY: Every allocation and deallocation is forwarded unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[tokio::test]
async fn cross_protocol_scopes_restore_the_authoritative_metainfo() {
    let grpc = volo_grpc::streaming::StreamContext::new("/S/M", None, Role::Server, None);
    let thrift = volo_thrift::streaming::StreamContext::new(1, "S", "M", Role::Server);
    grpc.update_metainfo(|info| info.set_persistent("owner", "grpc"));
    thrift.update_metainfo(|info| info.set_persistent("owner", "thrift"));
    grpc.sync_scope(|| {
        thrift.sync_scope(|| {
            grpc.shared_context().sync_scope(|| {
                assert_eq!(
                    metainfo::METAINFO
                        .with(|info| info.borrow().get_persistent("owner"))
                        .as_deref(),
                    Some("grpc")
                );
                assert_eq!(
                    volo::streaming::StreamContext::current().unwrap().id(),
                    grpc.shared_context().id()
                );
            });
            assert_eq!(
                metainfo::METAINFO
                    .with(|info| info.borrow().get_persistent("owner"))
                    .as_deref(),
                Some("thrift")
            );
            grpc.sync_scope(|| {
                assert_eq!(
                    metainfo::METAINFO
                        .with(|info| info.borrow().get_persistent("owner"))
                        .as_deref(),
                    Some("grpc")
                );
            });
        });
    });
}

fn start() -> Instant {
    ALLOCATIONS.with(|count| count.set(Some(0)));
    Instant::now()
}

#[tokio::test]
async fn metainfo_only_reentry_restores_cross_protocol_scopes() {
    struct Scope(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl volo::streaming::StreamScope for Scope {
        fn enter(&self, cx: &volo::streaming::StreamContext, operation: &mut dyn FnMut()) {
            assert_eq!(
                volo::streaming::StreamContext::current().unwrap().id(),
                cx.id()
            );
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            operation();
        }
    }
    let grpc = volo_grpc::streaming::StreamContext::new("/S/M", None, Role::Server, None);
    let thrift = volo_thrift::streaming::StreamContext::new(-1, "S", "M", Role::Server);
    let entries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    grpc.shared_context()
        .set_scope(std::sync::Arc::new(Scope(entries.clone())));
    thrift
        .shared_context()
        .set_scope(std::sync::Arc::new(Scope(entries.clone())));
    for shared in [false, true] {
        grpc.sync_scope(|| {
            thrift.sync_scope(|| {
                grpc.with_metainfo(|_| {
                    let before = entries.load(std::sync::atomic::Ordering::Relaxed);
                    let check = || {
                        assert_eq!(
                            volo::streaming::StreamContext::current().unwrap().id(),
                            grpc.shared_context().id()
                        );
                        assert_eq!(
                            entries.load(std::sync::atomic::Ordering::Relaxed),
                            before + 1
                        );
                    };
                    if shared {
                        grpc.shared_context().sync_scope(check);
                    } else {
                        grpc.sync_scope(check);
                    }
                    assert_eq!(
                        volo::streaming::StreamContext::current().unwrap().id(),
                        thrift.shared_context().id()
                    );
                })
            })
        });
        thrift.sync_scope(|| {
            grpc.sync_scope(|| {
                thrift.with_metainfo(|_| {
                    let before = entries.load(std::sync::atomic::Ordering::Relaxed);
                    let check = || {
                        assert_eq!(
                            volo::streaming::StreamContext::current().unwrap().id(),
                            thrift.shared_context().id()
                        );
                        assert_eq!(
                            entries.load(std::sync::atomic::Ordering::Relaxed),
                            before + 1
                        );
                    };
                    if shared {
                        thrift.shared_context().sync_scope(check);
                    } else {
                        thrift.sync_scope(check);
                    }
                    assert_eq!(
                        volo::streaming::StreamContext::current().unwrap().id(),
                        grpc.shared_context().id()
                    );
                })
            })
        });
    }
}
fn report(name: &str, started: Instant, iterations: usize) {
    let elapsed = started.elapsed();
    let allocations = ALLOCATIONS.with(|count| count.replace(None)).unwrap();
    println!(
        "{name}: iterations={iterations}, allocations={allocations}, ns/op={:.1}",
        elapsed.as_nanos() as f64 / iterations as f64
    );
}

/// Isolated current-thread allocation counts and timing, with warmup. Run on
/// both revisions using the same compiler/profile; this is not a network QPS test.
#[tokio::test(flavor = "current_thread")]
#[ignore = "manual allocation/latency comparison"]
async fn hot_path_costs() {
    const N: usize = 10_000;
    let scope = MetaInfoScope::default();
    scope.sync_scope(|| {});
    let started = start();
    for _ in 0..N {
        scope.sync_scope(|| std::hint::black_box(()));
    }
    report("metainfo_reentry", started, N);
    let started = start();
    for _ in 0..N {
        MetaInfoScope::default().sync_scope(|| std::hint::black_box(()));
    }
    report("metainfo_first_entry", started, N);

    let message = pilota::thrift::ApplicationException::new(
        pilota::thrift::ApplicationExceptionKind::INTERNAL_ERROR,
        "sample",
    );
    let cx = volo_grpc::streaming::StreamContext::new("/S/M", None, Role::Server, None);
    for shared in [false, true] {
        let (mut sender, source) = volo_grpc::thrift::channel(1);
        let mut body = source.into_body(
            None,
            volo_grpc::message::CodecOptions::new(Default::default()),
            None,
        );
        if shared {
            let mut sender = volo::streaming::ResponseSender::new(cx.shared_context(), sender);
            cx.scope(async {
                sender.send(&message).await.unwrap();
                body.next().await.unwrap().unwrap();
                let started = start();
                for _ in 0..N {
                    sender.send(&message).await.unwrap();
                    body.next().await.unwrap().unwrap();
                }
                report("grpc_shared_send", started, N);
            })
            .await;
        } else {
            sender.send(&message).await.unwrap();
            body.next().await.unwrap().unwrap();
            let started = start();
            for _ in 0..N {
                sender.send(&message).await.unwrap();
                body.next().await.unwrap().unwrap();
            }
            report("grpc_native_send", started, N);
        }
    }

    use volo_thrift::{
        streaming::StreamConfig,
        transport::ttstream::connection::{Connection, StreamRouter},
    };
    let (client, server) = tokio::io::duplex(64 * 1024);
    let mut router = StreamRouter::new();
    router
        .add("S", "Echo", |_, mut stream| async move {
            while let Some(bytes) = stream.recv_raw().await? {
                stream.send_raw(bytes).await?;
            }
            Ok(())
        })
        .unwrap();
    let (read, write) = tokio::io::split(server);
    let server =
        Connection::start(read, write, StreamConfig::default(), None, Some(router)).unwrap();
    let (read, write) = tokio::io::split(client);
    let client = Connection::start(read, write, StreamConfig::default(), None, None).unwrap();
    let mut stream = client.open("S", "Echo", Default::default()).await.unwrap();
    let payload = Bytes::from_static(b"payload");
    for _ in 0..100 {
        stream.send_raw(payload.clone()).await.unwrap();
        stream.recv_raw().await.unwrap().unwrap();
    }
    let started = start();
    for _ in 0..N {
        stream.send_raw(payload.clone()).await.unwrap();
        stream.recv_raw().await.unwrap().unwrap();
    }
    report("ttstream_raw_echo", started, N);
    stream.close_send().await.unwrap();
    assert!(stream.recv_raw().await.unwrap().is_none());
    client.close();
    server.close();
}
