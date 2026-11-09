//! Pool and control-frame behavior with live long streams, independently of any payload codec.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures::stream;
use http_body::Frame;
use http_body_util::BodyExt;
use motore::Service;
use tokio_util::task::AbortOnDropHandle;
use volo::{
    context::{Context as _, Endpoint, Role, RpcInfo},
    net::Address,
};
use volo_grpc::{
    BoxStream, Request, Response, Status,
    body::BoxBody,
    client::{Http2Config, StreamPoolConfig},
    codec::{compression::CompressionEncoding, decode::Kind},
    context::{ClientContext, Config, Deadline},
    message::{RecvEntryMessage, SendEntryMessage},
    streaming::StreamContext,
    transport::ClientTransport,
};

struct Empty;
impl SendEntryMessage for Empty {
    fn into_body(
        self,
        _: Option<CompressionEncoding>,
    ) -> BoxStream<'static, Result<Frame<Bytes>, Status>> {
        Box::pin(stream::empty())
    }
}
struct Reply(BoxBody);
impl RecvEntryMessage for Reply {
    fn from_body(
        _: Option<&str>,
        body: BoxBody,
        _: Kind,
        _: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Ok(Self(body))
    }
}
fn context(address: Address) -> ClientContext {
    let mut callee = Endpoint::new("pool-peer".into());
    callee.set_address(address);
    ClientContext::new(RpcInfo::new(
        Role::Client,
        "/pool.Peer/Call".into(),
        Endpoint::new("test".into()),
        callee,
        Config::default(),
    ))
}
async fn call(
    transport: &ClientTransport<Reply>,
    address: Address,
    hold: bool,
    deadline: Option<tokio::time::Instant>,
) -> Result<Response<Reply>, Status> {
    let mut request = Request::new(Empty);
    if hold {
        request
            .metadata_mut()
            .insert("x-hold", "1".parse().unwrap());
    }
    if let Some(deadline) = deadline {
        request.extensions_mut().insert(Deadline::new(deadline));
    } else {
        request.extensions_mut().insert(StreamContext::new(
            "/pool.Peer/Call",
            None,
            Role::Client,
            None,
        ));
    }
    transport.call(&mut context(address), request).await
}
async fn consume(response: Response<Reply>) {
    let mut body = response.into_inner().0;
    while let Some(frame) = body.frame().await {
        frame.unwrap();
    }
}

enum PeerListener {
    Tcp(tokio::net::TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
}
impl PeerListener {
    async fn accept(&self) -> std::io::Result<volo::net::conn::Conn> {
        match self {
            Self::Tcp(listener) => Ok(listener.accept().await?.0.into()),
            #[cfg(unix)]
            Self::Unix(listener) => Ok(listener.accept().await?.0.into()),
        }
    }
}

struct Peer {
    address: Address,
    accepted: Arc<AtomicUsize>,
    submitted: Arc<AtomicUsize>,
    held: Arc<Mutex<Vec<h2::SendStream<Bytes>>>>,
    drain: tokio::sync::watch::Sender<()>,
    _task: AbortOnDropHandle<()>,
    _directory: Option<tempfile::TempDir>,
}
impl Peer {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = Address::Ip(listener.local_addr().unwrap());
        Self::listen(PeerListener::Tcp(listener), address, None)
    }
    #[cfg(unix)]
    async fn new_unix() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("grpc-pool-")
            .tempdir()
            .unwrap();
        let path = directory.path().join("p.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let address = Address::Unix(std::os::unix::net::SocketAddr::from_pathname(path).unwrap());
        Self::listen(PeerListener::Unix(listener), address, Some(directory))
    }
    fn listen(
        listener: PeerListener,
        address: Address,
        directory: Option<tempfile::TempDir>,
    ) -> Self {
        let accepted = Arc::new(AtomicUsize::new(0));
        let submitted = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(Mutex::new(Vec::new()));
        let (count, requests, sends) = (accepted.clone(), submitted.clone(), held.clone());
        let (drain, drained) = tokio::sync::watch::channel(());
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    socket = listener.accept() => {
                        let socket = socket.unwrap();
                        count.fetch_add(1, Ordering::SeqCst);
                        let (requests, sends) = (requests.clone(), sends.clone());
                        let mut drained = drained.clone();
                        drained.borrow_and_update();
                        tasks.spawn(async move {
                            let mut connection = h2::server::Builder::new().max_concurrent_streams(1).handshake::<_, Bytes>(socket).await.unwrap();
                            let mut draining = false;
                            let mut request_bodies = Vec::new();
                            loop {
                                let request = tokio::select! {
                                    _ = drained.changed(), if !draining => { draining = true; connection.graceful_shutdown(); continue; },
                                    request = connection.accept() => request,
                                };
                                let Some(Ok((request, mut response))) = request else { break };
                                requests.fetch_add(1, Ordering::SeqCst);
                                let hold = request.headers().contains_key("x-hold");
                                request_bodies.push(request.into_body());
                                let mut builder = http::Response::builder().header("content-type", "application/grpc");
                                if !hold { builder = builder.header("grpc-status", "0"); }
                                let sent = response.send_response(builder.body(()).unwrap(), !hold).unwrap();
                                if hold { sends.lock().unwrap().push(sent); }
                            }
                        });
                    },
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); },
                }
            }
        });
        Self {
            address,
            accepted,
            submitted,
            held,
            drain,
            _task: AbortOnDropHandle::new(task),
            _directory: directory,
        }
    }
    fn release(&self) {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", "0".parse().unwrap());
        for mut send in self.held.lock().unwrap().drain(..) {
            let _ = send.send_trailers(trailers.clone());
        }
    }
}

#[tokio::test]
async fn long_streams_allow_bounded_pool_growth_and_expired_queue_entries_never_reach_wire() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let peer = Peer::new().await;
        let mut pool = StreamPoolConfig::default();
        pool.max_connections_per_endpoint = 2;
        pool.max_pending_requests = 2;
        let transport = ClientTransport::new(
            &Http2Config::default().with_stream_pool_config(pool),
            &Config::default(),
        );
        let first = call(&transport, peer.address.clone(), true, None)
            .await
            .unwrap();
        let tick = tokio::time::Instant::now();
        consume(
            call(&transport, peer.address.clone(), false, None)
                .await
                .unwrap(),
        )
        .await;
        assert!(tick.elapsed() < Duration::from_millis(500));
        assert_eq!(peer.accepted.load(Ordering::SeqCst), 2);
        let second = call(&transport, peer.address.clone(), true, None)
            .await
            .unwrap();
        let error = call(
            &transport,
            peer.address.clone(),
            false,
            Some(tokio::time::Instant::now() + Duration::from_millis(60)),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code(), volo_grpc::Code::DeadlineExceeded);
        assert_eq!(peer.accepted.load(Ordering::SeqCst), 2);
        assert_eq!(peer.submitted.load(Ordering::SeqCst), 3);
        peer.release();
        consume(first).await;
        consume(second).await;
        consume(
            call(&transport, peer.address.clone(), false, None)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(peer.submitted.load(Ordering::SeqCst), 4);
        assert_eq!(peer.accepted.load(Ordering::SeqCst), 2);
    })
    .await
    .expect("bounded pool, queue deadline and long-stream cleanup");
}

#[tokio::test]
async fn active_idle_bidi_survives_multiple_configured_keepalive_periods() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let peer = Peer::new().await;
        let config = Http2Config::default().with_keepalive(
            Some(Duration::from_millis(25)),
            Duration::from_millis(150),
            false,
        );
        let transport = ClientTransport::new(&config, &Config::default());
        let response = call(&transport, peer.address.clone(), true, None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(180)).await;
        peer.release();
        consume(response).await;
        consume(
            call(&transport, peer.address.clone(), false, None)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(peer.accepted.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("idle active streams survive valid keepalive");
}

#[tokio::test]
async fn keepalive_without_active_calls_is_opt_in() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let interval = Duration::from_millis(25);
        for (ping, while_idle) in [
            (None, false),
            (Some(interval), false),
            (Some(interval), true),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = Address::Ip(listener.local_addr().unwrap());
            let (resume, idle_elapsed) = tokio::sync::oneshot::channel();
            let peer = AbortOnDropHandle::new(tokio::spawn(async move {
                let reply = |mut send: h2::server::SendResponse<Bytes>| {
                    send.send_response(
                        http::Response::builder()
                            .header("content-type", "application/grpc")
                            .header("grpc-status", "0")
                            .body(())
                            .unwrap(),
                        true,
                    )
                    .unwrap();
                };
                let (socket, _) = listener.accept().await.unwrap();
                let mut conn = h2::server::handshake(socket).await.unwrap();
                let (_, send) = conn.accept().await.unwrap().unwrap();
                reply(send);
                // Flush the response, then keep the socket open without polling
                // the connection to ACK any idle PINGs.
                let flush = Duration::from_millis(10);
                assert!(tokio::time::timeout(flush, conn.accept()).await.is_err());
                tokio::time::sleep(Duration::from_millis(300)).await;
                resume.send(()).unwrap();
                let mut conn = if while_idle {
                    let (fresh, _) = listener.accept().await.unwrap();
                    h2::server::handshake(fresh).await.unwrap()
                } else {
                    conn
                };
                let (_, send) = conn.accept().await.unwrap().unwrap();
                reply(send);
                let _ = tokio::time::timeout(flush, conn.accept()).await;
            }));
            let config =
                Http2Config::default().with_keepalive(ping, Duration::from_millis(50), while_idle);
            let transport = ClientTransport::new(&config, &Config::default());
            consume(
                call(&transport, address.clone(), false, None)
                    .await
                    .unwrap(),
            )
            .await;
            idle_elapsed.await.unwrap();
            consume(call(&transport, address, false, None).await.unwrap()).await;
            peer.await.unwrap();
        }
    })
    .await
    .expect("only explicitly enabled idle PINGs retire an unresponsive idle connection");
}

#[tokio::test]
async fn oversized_business_metadata_is_rejected_at_the_h2_boundary() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for limit in [None, Some(32 * 1024)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = Address::Ip(listener.local_addr().unwrap());
            let _peer = AbortOnDropHandle::new(tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(socket).await.unwrap();
                let (_, mut send) = connection.accept().await.unwrap().unwrap();
                let mut extra = b"{\"detail\":\"".to_vec();
                // Exceed the default receive limit with well-formed JSON.
                extra.resize(16 * 1024, b'x');
                extra.extend_from_slice(b"\"}");
                let extra = http::HeaderValue::from_maybe_shared(Bytes::from(extra)).unwrap();
                let response = http::Response::builder()
                    .header("content-type", "application/grpc")
                    .header("grpc-status", "13")
                    .header("biz-status", "409")
                    .header("biz-extra", extra)
                    .body(())
                    .unwrap();
                send.send_response(response, true).unwrap();
                let _ = connection.accept().await;
            }));
            let config = limit.map_or_else(Http2Config::default, |limit| {
                Http2Config::default().with_max_header_list_size(limit)
            });
            let transport = ClientTransport::new(&config, &Config::default());
            let error = call(&transport, address, false, None)
                .await
                .err()
                .expect("peer business failure or header limit");
            assert_eq!(error.code(), volo_grpc::Code::Internal);
            if limit.is_some() {
                assert_eq!(error.raw_grpc_code(), Some(b"13".as_slice()));
                let extra = error.metadata().get("biz-extra").unwrap().as_bytes();
                assert_eq!(extra.len(), 16 * 1024 + 2);
            } else {
                assert!(error.raw_grpc_code().is_none());
                assert!(error.message().contains("h2 protocol error"), "{error:?}");
            }
        }
    })
    .await
    .expect("oversized headers must terminate the call");
}

#[tokio::test]
async fn goaway_retires_admission_without_replaying_or_aborting_an_existing_stream() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let peer = Peer::new().await;
        let transport = ClientTransport::new(&Http2Config::default(), &Config::default());
        let first = call(&transport, peer.address.clone(), true, None)
            .await
            .unwrap();
        let owner = first.extensions().get::<StreamContext>().unwrap().clone();
        peer.drain.send(()).unwrap();
        // Let the two-stage graceful GOAWAY/PING exchange cross the loopback socket.
        tokio::time::sleep(Duration::from_millis(60)).await;
        consume(
            call(&transport, peer.address.clone(), false, None)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(peer.accepted.load(Ordering::SeqCst), 2);
        assert_eq!(
            peer.submitted.load(Ordering::SeqCst),
            2,
            "submitted HEADERS must not be replayed"
        );
        assert!(
            owner.status().is_none(),
            "an accepted long stream continues while its connection drains"
        );
        peer.release();
        consume(first).await;
        owner.children_reclaimed().await;
        assert_eq!(owner.status().unwrap().status.code(), volo_grpc::Code::Ok);
    })
    .await
    .expect("GOAWAY retirement is bounded and preserves accepted calls");
}

#[tokio::test]
async fn missing_keepalive_ack_releases_the_call_with_a_transport_failure() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = AbortOnDropHandle::new(tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(socket).await.unwrap();
            let (_request, mut response) = connection.accept().await.unwrap().unwrap();
            let _send = response
                .send_response(
                    http::Response::builder()
                        .header("content-type", "application/grpc")
                        .body(())
                        .unwrap(),
                    false,
                )
                .unwrap();
            let _ = tokio::time::timeout(Duration::from_millis(10), connection.accept()).await;
            // Keep the socket and stream open, but stop driving PING acknowledgements.
            tokio::time::sleep(Duration::from_millis(500)).await;
            drop(connection);
        }));
        let config = Http2Config::default().with_keepalive(
            Some(Duration::from_millis(40)),
            Duration::from_millis(100),
            false,
        );
        let transport = ClientTransport::new(&config, &Config::default());
        let response = call(&transport, Address::Ip(address), true, None)
            .await
            .unwrap();
        let owner = response
            .extensions()
            .get::<StreamContext>()
            .unwrap()
            .clone();
        let mut body = response.into_inner().0;
        let frame = body.frame().await;
        let status = match frame {
            Some(Err(status)) => status,
            Some(Ok(frame)) => volo_grpc::Status::from_header_map(
                frame.trailers_ref().expect("terminal control frame"),
            )
            .expect("terminal status"),
            other => panic!(
                "expected keepalive I/O error, got {other:?}; terminal={:?}",
                owner.status()
            ),
        };
        assert_eq!(status.code(), volo_grpc::Code::Unavailable);
        assert!(status.message().contains("keepalive"), "{status:?}");
        owner.children_reclaimed().await;
        drop(peer);
    })
    .await
    .expect("unacknowledged PING cancels the owned I/O task");
}

#[cfg(unix)]
#[tokio::test]
async fn unix_dial_address_is_not_sent_as_an_http2_scheme() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let directory = tempfile::Builder::new()
            .prefix("grpc-scheme-")
            .tempdir()
            .unwrap();
        let path = directory.path().join("rpc.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(socket).await.unwrap();
            let (request, mut respond) = connection.accept().await.unwrap().unwrap();
            assert_eq!(request.uri().scheme_str(), Some("http"));
            respond
                .send_response(
                    http::Response::builder()
                        .header("content-type", "application/grpc")
                        .header("grpc-status", "0")
                        .body(())
                        .unwrap(),
                    true,
                )
                .unwrap();
            drop((request, respond));
            tokio::pin!(stopped);
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    request = connection.accept() => if request.is_none() { break },
                }
            }
        });
        let transport = ClientTransport::new(&Http2Config::default(), &Config::default());
        let mut cx = context(Address::Ip("127.0.0.1:1".parse().unwrap()));
        cx.rpc_info_mut().callee_mut().set_address(Address::Unix(
            std::os::unix::net::SocketAddr::from_pathname(path).unwrap(),
        ));
        let mut request = Request::new(Empty);
        request.extensions_mut().insert(StreamContext::new(
            "/pool.Peer/Call",
            None,
            Role::Client,
            None,
        ));
        let response =
            tokio::time::timeout(Duration::from_secs(1), transport.call(&mut cx, request))
                .await
                .expect("response Headers")
                .unwrap();
        tokio::time::timeout(Duration::from_secs(1), consume(response))
            .await
            .expect("terminal body");
        stop.send(()).unwrap();
        drop(transport);
        tokio::time::timeout(Duration::from_secs(1), peer)
            .await
            .expect("peer shutdown")
            .unwrap();
    })
    .await
    .expect("wire :scheme is independent of Unix dialing");
}

/// A controlled pool measurement, not a payload throughput comparison. The
/// peer permits one active stream per connection so both saturation states are
/// observable without hundreds of long-lived application tasks.
#[cfg(unix)]
#[tokio::test]
async fn short_rpc_latency_with_partially_and_fully_occupied_long_stream_pool() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for unix in [false, true] {
            for repetition in 0..3 {
                let peer = if unix {
                    Peer::new_unix().await
                } else {
                    Peer::new().await
                };
                let mut pool = StreamPoolConfig::default();
                pool.max_connections_per_endpoint = 2;
                pool.max_pending_requests = 2;
                pool.idle_timeout = Duration::from_millis(200);
                let transport = ClientTransport::new(
                    &Http2Config::default().with_stream_pool_config(pool),
                    &Config::default(),
                );
                let tick = tokio::time::Instant::now();
                consume(
                    call(&transport, peer.address.clone(), false, None)
                        .await
                        .unwrap(),
                )
                .await;
                let cold_us = tick.elapsed().as_micros();
                let first = call(&transport, peer.address.clone(), true, None)
                    .await
                    .unwrap();
                let mut short_us = Vec::new();
                for _ in 0..64 {
                    let tick = tokio::time::Instant::now();
                    consume(
                        call(&transport, peer.address.clone(), false, None)
                            .await
                            .unwrap(),
                    )
                    .await;
                    short_us.push(tick.elapsed().as_micros());
                }
                short_us.sort_unstable();
                assert_eq!(peer.accepted.load(Ordering::SeqCst), 2);
                assert!(
                    short_us[63] < 500_000,
                    "one held stream must leave short RPC capacity"
                );
                let second = call(&transport, peer.address.clone(), true, None)
                    .await
                    .unwrap();
                let submitted = peer.submitted.load(Ordering::SeqCst);
                let tick = tokio::time::Instant::now();
                let queued = async {
                    consume(
                        call(&transport, peer.address.clone(), false, None)
                            .await
                            .unwrap(),
                    )
                    .await;
                    tick.elapsed().as_micros()
                };
                let release = async {
                    tokio::time::sleep(Duration::from_millis(70)).await;
                    assert_eq!(peer.accepted.load(Ordering::SeqCst), 2);
                    assert_eq!(peer.submitted.load(Ordering::SeqCst), submitted);
                    peer.release();
                };
                let (queued_us, ()) = tokio::join!(queued, release);
                assert!((50_000..500_000).contains(&queued_us));
                consume(first).await;
                consume(second).await;
                let held = call(&transport, peer.address.clone(), true, None)
                    .await
                    .unwrap();
                drop(held);
                consume(
                    call(&transport, peer.address.clone(), false, None)
                        .await
                        .unwrap(),
                )
                .await;
                assert_eq!(
                    peer.accepted.load(Ordering::SeqCst),
                    2,
                    "cancel reuses the healthy pool"
                );
                tokio::time::sleep(Duration::from_millis(450)).await;
                consume(
                    call(&transport, peer.address.clone(), false, None)
                        .await
                        .unwrap(),
                )
                .await;
                assert_eq!(
                    peer.accepted.load(Ordering::SeqCst),
                    3,
                    "idle connections retire before redial"
                );
                let transport = if unix { "uds" } else { "tcp" };
                println!(
                    "transport={transport} repetition={repetition} cold_us={cold_us} \
                     short_p50_us={} short_p99_us={} saturated_queue_us={queued_us} \
                     connections_before_idle=2 accepted_after_idle=3 samples={}",
                    short_us[31],
                    short_us[63],
                    short_us.len(),
                );
            }
        }
    })
    .await
    .expect("short RPCs, full queues, cancellation and idle retirement are bounded");
}
