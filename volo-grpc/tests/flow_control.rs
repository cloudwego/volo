//! Observe actual send-window waiting; a producer delay is not flow control.
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
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
    body::{Body, BoxBody, boxed},
    context::ServerContext,
    server::{NamedService, Server},
    streaming::{StreamContext, StreamHook},
};

#[derive(Clone)]
struct Probe;
impl NamedService for Probe {
    const NAME: &'static str = "flow.Probe";
}
impl Service<ServerContext, Request<BoxBody>> for Probe {
    type Response = Response<BoxBody>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        _: Request<BoxBody>,
    ) -> Result<Self::Response, Status> {
        Ok(Response::new(boxed(Body::new(Box::pin(stream::iter([
            Ok(Frame::data(Bytes::from_static(b"\0\0\0\0\0"))),
        ]))))))
    }
}
#[derive(Clone, Default)]
struct Capture {
    calls: Arc<AtomicUsize>,
    micros: Arc<AtomicU64>,
}
impl StreamHook for Capture {
    fn on_flow_control(&self, _: &StreamContext, wait: Duration) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.micros
            .fetch_add(wait.as_micros() as u64, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn zero_window_wait_is_reported_to_both_registered_hooks_when_it_resumes() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let first = Capture::default();
        let second = Capture::default();
        let (a, b) = (first.clone(), second.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            Server::new()
                .add_service(Probe)
                .stream_hook(move |_: &StreamContext| a.clone())
                .stream_hook(move |_: &StreamContext| b.clone())
                .run_with_shutdown(DefaultIncoming::from(listener), async {
                    let _ = stopped.await;
                    Ok(())
                })
                .await
                .unwrap();
        });
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, mut connection) = h2::client::Builder::new()
            .initial_window_size(0)
            .handshake::<_, Bytes>(socket)
            .await
            .unwrap();
        let (update, updated) = tokio::sync::oneshot::channel();
        let io = tokio::spawn(async move {
            tokio::select! {
                result = &mut connection => { result.unwrap(); return; },
                _ = updated => connection.set_initial_window_size(1024).unwrap(),
            }
            let _ = connection.await;
        });
        let (response, _) = client
            .send_request(
                http::Request::builder()
                    .method("POST")
                    .uri(format!("http://{address}/flow.Probe/Call"))
                    .header("content-type", "application/grpc")
                    .body(())
                    .unwrap(),
                true,
            )
            .unwrap();
        let mut response = response.await.unwrap().into_body();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(first.calls.load(Ordering::Relaxed), 0);
        update.send(()).unwrap();
        let bytes = response.data().await.unwrap().unwrap();
        assert_eq!(bytes.as_ref(), b"\0\0\0\0\0");
        response
            .flow_control()
            .release_capacity(bytes.len())
            .unwrap();
        assert!(response.data().await.is_none());
        assert_eq!(
            response.trailers().await.unwrap().unwrap()["grpc-status"],
            "0"
        );
        for capture in [first, second] {
            assert_eq!(capture.calls.load(Ordering::Relaxed), 1);
            assert!(capture.micros.load(Ordering::Relaxed) >= 20_000);
        }
        drop(client);
        io.abort();
        let _ = io.await;
        stop.send(()).unwrap();
        server.await.unwrap();
    })
    .await
    .expect("flow-control observation must not block termination");
}
