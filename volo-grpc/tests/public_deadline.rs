//! Public-path regressions: no Thrift feature or generated service required.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures::stream;
use http_body::Frame;
use motore::Service;
use volo::net::incoming::DefaultIncoming;
use volo_grpc::{
    Request, Response, Status,
    body::{Body, BoxBody, boxed, empty_body},
    context::ServerContext,
    server::{MetaService, NamedService, Server},
    tracing::DefaultProvider,
};

#[derive(Clone)]
struct Probe(Arc<AtomicUsize>);

impl NamedService for Probe {
    const NAME: &'static str = "public.Probe";
}

impl Service<ServerContext, Request<BoxBody>> for Probe {
    type Response = Response<BoxBody>;
    type Error = Status;

    async fn call(
        &self,
        _: &mut ServerContext,
        _: Request<BoxBody>,
    ) -> Result<Self::Response, Status> {
        self.0.fetch_add(1, Ordering::Relaxed);
        // A single empty Protobuf message followed by the normal success Trailer.
        Ok(Response::new(boxed(Body::new(Box::pin(stream::iter([
            Ok(Frame::data(Bytes::from_static(b"\0\0\0\0\0"))),
        ]))))))
    }
}

#[tokio::test]
async fn queued_request_does_not_restart_the_received_budget() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut service = MetaService::new(
        volo_grpc::server::layer::timeout::Timeout::new(Probe(calls.clone())),
        DefaultProvider,
    );
    let request = http::Request::builder()
        .uri("/public.Probe/Unary")
        .header("grpc-timeout", "1m")
        .body(empty_body())
        .unwrap();
    let future = tower::Service::call(&mut service, request);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let response = future.await.unwrap();
    assert_eq!(response.headers()["grpc-status"], "4");
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn zero_budget_is_trailer_only_and_does_not_poison_other_h2_calls() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = Server::new().add_service(Probe(calls.clone()));
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            server
                .run_with_shutdown(DefaultIncoming::from(listener), async {
                    stop_rx.await.unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, connection) = h2::client::handshake(socket).await.unwrap();
        let connection_task = tokio::spawn(connection);
        for timeout in [Some("0n"), Some("500m"), None] {
            let mut builder = http::Request::builder()
                .method("POST")
                .uri(format!("http://{address}/public.Probe/Unary"))
                .header("content-type", "application/grpc");
            if let Some(value) = timeout {
                builder = builder.header("grpc-timeout", value);
            }
            let (response, mut send) = client
                .send_request(builder.body(()).unwrap(), false)
                .unwrap();
            send.send_data(Bytes::from_static(b"\0\0\0\0\0"), true)
                .unwrap();
            let response = response.await.unwrap();
            assert_eq!(response.headers()["content-type"], "application/grpc");
            let (headers, mut body) = response.into_parts();
            if timeout == Some("0n") {
                assert_eq!(headers.headers["grpc-status"], "4");
                assert!(body.data().await.is_none());
                assert_eq!(calls.load(Ordering::Relaxed), 0);
            } else {
                let data = body.data().await.unwrap().unwrap();
                assert_eq!(data.as_ref(), b"\0\0\0\0\0");
                body.flow_control().release_capacity(data.len()).unwrap();
                assert!(body.data().await.is_none());
                assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
            }
        }
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        drop(client);
        connection_task.abort();
        let _ = connection_task.await;
        stop_tx.send(()).unwrap();
        server_task.await.unwrap();
    })
    .await
    .expect("H2 deadline regression must finish within its budget");
}
