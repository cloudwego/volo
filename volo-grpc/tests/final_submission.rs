//! C3 wire gate: the budget must be serialized after waiting for an H2 slot.
use std::{net::SocketAddr, time::Duration};

use bytes::Bytes;
use futures::stream;
use http_body::Frame;
use motore::Service;
use tokio_util::task::AbortOnDropHandle;
use volo::{
    context::{Endpoint, Role, RpcInfo},
    net::{Address, incoming::DefaultIncoming},
};
use volo_grpc::{
    BoxStream, RecvEntryMessage, Request, Response, SendEntryMessage, Status,
    body::{Body, BoxBody, boxed},
    client::{Http2Config, layer::timeout::Timeout},
    codec::{compression::CompressionEncoding, decode::Kind},
    context::{ClientContext, Config, Deadline, ServerContext},
    server::{NamedService, Server},
    transport::ClientTransport,
};

struct EmptySend;
impl SendEntryMessage for EmptySend {
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

fn context(address: std::net::SocketAddr) -> ClientContext {
    let mut endpoint = Endpoint::new("slot-peer".into());
    endpoint.set_address(Address::Ip(address));
    ClientContext::new(RpcInfo::new(
        Role::Client,
        "/slot.Peer/Call".into(),
        Endpoint::new("test".into()),
        endpoint,
        Config::default(),
    ))
}

fn parse_timeout(value: &http::HeaderValue) -> Duration {
    let value = value.to_str().unwrap();
    let (digits, unit) = value.split_at(value.len() - 1);
    let value: u64 = digits.parse().unwrap();
    match unit {
        "H" => Duration::from_secs(value * 3600),
        "M" => Duration::from_secs(value * 60),
        "S" => Duration::from_secs(value),
        "m" => Duration::from_millis(value),
        "u" => Duration::from_micros(value),
        "n" => Duration::from_nanos(value),
        _ => panic!("invalid grpc-timeout unit"),
    }
}

#[derive(Clone)]
struct Relay {
    target: SocketAddr,
    client: Timeout<ClientTransport<Reply>>,
}
impl NamedService for Relay {
    const NAME: &'static str = "slot.Peer";
}
impl Service<ServerContext, Request<BoxBody>> for Relay {
    type Response = Response<BoxBody>;
    type Error = Status;

    async fn call(
        &self,
        _: &mut ServerContext,
        _: Request<BoxBody>,
    ) -> Result<Self::Response, Status> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.client
            .call(&mut context(self.target), Request::new(EmptySend))
            .await?;
        Ok(Response::new(boxed(Body::new(Box::pin(stream::iter([
            Ok(Frame::data(Bytes::from_static(b"\0\0\0\0\0"))),
        ]))))))
    }
}

#[tokio::test]
async fn protobuf_relay_inherits_the_received_deadline() {
    tokio::time::timeout(Duration::from_secs(6), async {
        for budget in [Some(Duration::from_secs(2)), None] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = listener.local_addr().unwrap();
            let (observed, observation) = tokio::sync::oneshot::channel();
            let _peer = AbortOnDropHandle::new(tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(socket).await.unwrap();
                let (request, mut respond) = connection.accept().await.unwrap().unwrap();
                let budgets: Vec<_> = request
                    .headers()
                    .get_all("grpc-timeout")
                    .iter()
                    .map(parse_timeout)
                    .collect();
                observed
                    .send((budgets, tokio::time::Instant::now()))
                    .unwrap();
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
                let _ = connection.accept().await;
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let transport =
                ClientTransport::<Reply>::new(&Http2Config::default(), &Config::default());
            let relay = Relay {
                target,
                client: Timeout::new(transport.clone()),
            };
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let server = AbortOnDropHandle::new(tokio::spawn(async move {
                Server::new()
                    .add_service(relay)
                    .run_with_shutdown(DefaultIncoming::from(listener), async {
                        stopped.await.unwrap();
                        Ok(())
                    })
                    .await
                    .unwrap();
            }));
            let deadline = budget.map(|budget| tokio::time::Instant::now() + budget);
            let mut request = Request::new(EmptySend);
            if let Some(deadline) = deadline {
                request.extensions_mut().insert(Deadline::new(deadline));
            }
            let response = transport
                .call(&mut context(address), request)
                .await
                .unwrap();
            let (budgets, received_at) = observation.await.unwrap();
            if let Some((budget, deadline)) = budget.zip(deadline) {
                assert_eq!(
                    budgets.len(),
                    1,
                    "the downstream peer must see one grpc-timeout"
                );
                let advertised = budgets[0];
                assert!(
                    advertised < budget - Duration::from_millis(80),
                    "{advertised:?}"
                );
                assert!(
                    advertised
                        <= deadline.saturating_duration_since(received_at)
                            + Duration::from_millis(20)
                );
            } else {
                assert!(
                    budgets.is_empty(),
                    "a call without a deadline must not acquire one"
                );
            }
            drop((response, transport));
            stop.send(()).unwrap();
            server.await.unwrap();
        }
    })
    .await
    .expect("the three-node deadline probe must terminate");
}

#[tokio::test]
async fn outbound_timeout_deducts_waiting_for_a_peer_h2_stream_slot() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (observed, observation) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::Builder::new()
                .max_concurrent_streams(1)
                .handshake::<_, Bytes>(socket)
                .await
                .unwrap();
            let (_, mut first) = connection.accept().await.unwrap().unwrap();
            let mut hold = first
                .send_response(
                    http::Response::builder()
                        .header("content-type", "application/grpc")
                        .body(())
                        .unwrap(),
                    false,
                )
                .unwrap();
            let release = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let mut trailers = http::HeaderMap::new();
                trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
                hold.send_trailers(trailers).unwrap();
            });
            let (request, mut respond) = connection.accept().await.unwrap().unwrap();
            let remaining = parse_timeout(request.headers().get("grpc-timeout").unwrap());
            observed
                .send((remaining, tokio::time::Instant::now()))
                .unwrap();
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
            release.await.unwrap();
            connection.graceful_shutdown();
            while let Some(stream) = connection.accept().await {
                stream.unwrap();
            }
        });
        let mut pool = volo_grpc::client::StreamPoolConfig::default();
        pool.max_connections_per_endpoint = 1;
        let transport = ClientTransport::<Reply>::new(
            &Http2Config::default().with_stream_pool_config(pool),
            &Config::default(),
        );
        let mut first_cx = context(address);
        let mut first = Request::new(EmptySend);
        first
            .extensions_mut()
            .insert(volo_grpc::streaming::StreamContext::new(
                "/slot.Peer/Call",
                Some(volo_grpc::message::RpcMode::Bidirectional),
                Role::Client,
                None,
            ));
        let first = transport.call(&mut first_cx, first).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut second = Request::new(EmptySend);
        // The old public timeout layer writes the full value before this queue.
        second
            .metadata_mut()
            .insert("grpc-timeout", "1S".parse().unwrap());
        second.extensions_mut().insert(Deadline::new(deadline));
        let mut second_cx = context(address);
        let second = transport.call(&mut second_cx, second).await.unwrap();
        let (advertised, received_at) = observation.await.unwrap();
        let actual = deadline.saturating_duration_since(received_at);
        assert!(
            advertised <= actual + Duration::from_millis(20),
            "grpc-timeout overstated the remaining budget: advertised={advertised:?}, \
             actual={actual:?}"
        );
        drop((first.into_inner().0, second.into_inner().0, transport));
        peer.await.unwrap();
    })
    .await
    .expect("submission gate must be bounded");
}

#[tokio::test]
async fn outbound_timeout_deducts_layer_delay_and_slow_h2_handshake() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (observed, observation) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            let mut connection = h2::server::handshake::<_>(socket).await.unwrap();
            let (request, mut respond) = connection.accept().await.unwrap().unwrap();
            assert_eq!(request.headers().get_all("grpc-timeout").iter().count(), 1);
            observed
                .send((
                    parse_timeout(&request.headers()["grpc-timeout"]),
                    tokio::time::Instant::now(),
                ))
                .unwrap();
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
            connection.graceful_shutdown();
            while let Some(stream) = connection.accept().await {
                stream.unwrap();
            }
        });
        let transport = ClientTransport::<Reply>::new(&Http2Config::default(), &Config::default());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut request = Request::new(EmptySend);
        request.extensions_mut().insert(Deadline::new(deadline));
        request
            .metadata_mut()
            .insert("grpc-timeout", "1S".parse().unwrap());
        // A preceding motore layer can spend the inherited absolute budget
        // before entering the transport, independently of connection setup.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let response = transport
            .call(&mut context(address), request)
            .await
            .unwrap();
        let (advertised, received_at) = observation.await.unwrap();
        let actual = deadline.saturating_duration_since(received_at);
        assert!(actual < Duration::from_millis(800));
        assert!(
            advertised <= actual + Duration::from_millis(20),
            "advertised={advertised:?}, actual={actual:?}"
        );
        drop((response.into_inner().0, transport));
        peer.await.unwrap();
    })
    .await
    .expect("slow-handshake budget gate must finish");
}

#[tokio::test]
async fn deadline_expires_during_h2_handshake_without_committing_request_headers() {
    tokio::time::timeout(Duration::from_secs(4), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
            if let Ok(mut connection) = h2::server::handshake::<_>(socket).await {
                assert!(
                    !matches!(connection.accept().await, Some(Ok(_))),
                    "expired RPC reached peer"
                );
            }
        });
        let transport = ClientTransport::<Reply>::new(&Http2Config::default(), &Config::default());
        let mut request = Request::new(EmptySend);
        request.extensions_mut().insert(Deadline::new(
            tokio::time::Instant::now() + Duration::from_millis(80),
        ));
        let error = transport
            .call(&mut context(address), request)
            .await
            .err()
            .expect("handshake must exhaust the budget");
        assert_eq!(error.code(), volo_grpc::Code::DeadlineExceeded);
        drop(transport);
        peer.await.unwrap();
    })
    .await
    .expect("expired-handshake gate must finish");
}
