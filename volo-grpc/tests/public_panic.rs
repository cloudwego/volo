//! A panicking handler/producer is isolated to its RPC and reclaims its children.
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures::{Stream, stream};
use http_body::Frame;
use motore::Service;
use tokio::sync::Notify;
use volo::net::incoming::DefaultIncoming;
use volo_grpc::{
    Request, Response, Status,
    body::{Body, BoxBody, boxed},
    context::ServerContext,
    server::{NamedService, Server},
    streaming::{NoopHooks, StreamContext, StreamHookFactory},
};

type Child = tokio::task::JoinHandle<Result<(), Status>>;
#[derive(Clone)]
struct Probe {
    child: Arc<Mutex<Option<Child>>>,
    dropped: Arc<Notify>,
}
struct DropSignal(Arc<Notify>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
struct PanicProducer(DropSignal);
impl Stream for PanicProducer {
    type Item = Result<Frame<Bytes>, Status>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let _ = &self.0;
        panic!("intentional producer fixture panic")
    }
}
impl NamedService for Probe {
    const NAME: &'static str = "public.PanicProbe";
    fn stream_hook_factory(&self) -> Option<Arc<dyn StreamHookFactory>> {
        Some(Arc::new(|_: &StreamContext| NoopHooks))
    }
}
impl Service<ServerContext, Request<BoxBody>> for Probe {
    type Response = Response<BoxBody>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        request: Request<BoxBody>,
    ) -> Result<Self::Response, Status> {
        let phase = request
            .metadata()
            .get("x-panic-phase")
            .and_then(|value| value.to_str().ok());
        if let Some(phase) = phase {
            let signal = DropSignal(self.dropped.clone());
            let context = request.extensions().get::<StreamContext>().unwrap();
            let (ready, started) = tokio::sync::oneshot::channel();
            *self.child.lock().unwrap() = Some(context.spawn_scoped(async move {
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
            }));
            started.await.unwrap();
            if phase == "handler" {
                let _signal = signal;
                panic!("intentional handler fixture panic");
            }
            Ok(Response::new(boxed(Body::new(Box::pin(PanicProducer(
                signal,
            ))))))
        } else {
            Ok(Response::new(boxed(Body::new(Box::pin(stream::iter([
                Ok(Frame::data(Bytes::from_static(b"\0\0\0\0\0"))),
            ]))))))
        }
    }
}

#[tokio::test]
async fn handler_and_producer_panics_reclaim_children_and_preserve_the_h2_connection() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let child = Arc::new(Mutex::new(None));
        let dropped = Arc::new(Notify::new());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let probe = Probe {
            child: child.clone(),
            dropped: dropped.clone(),
        };
        let server = tokio::spawn(async move {
            Server::new()
                .add_service(probe)
                .run_with_shutdown(DefaultIncoming::from(listener), async {
                    stopped.await.unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, io) = h2::client::handshake(socket).await.unwrap();
        let io = tokio::spawn(io);
        for phase in [Some("handler"), None, Some("producer"), None] {
            let mut builder = http::Request::builder()
                .method("POST")
                .uri(format!("http://{address}/public.PanicProbe/Call"))
                .header("content-type", "application/grpc");
            if let Some(phase) = phase {
                builder = builder.header("x-panic-phase", phase);
            }
            let (response, mut send) = client
                .send_request(builder.body(()).unwrap(), false)
                .unwrap();
            send.send_data(Bytes::from_static(b"\0\0\0\0\0"), true)
                .unwrap();
            let response = response.await.unwrap();
            let initial = response.headers().get("grpc-status").cloned();
            let mut body = response.into_body();
            let mut count = 0;
            while let Some(data) = body.data().await {
                let data = data.unwrap();
                count += 1;
                body.flow_control().release_capacity(data.len()).unwrap();
            }
            let trailers = body.trailers().await.unwrap();
            let status =
                initial.or_else(|| trailers.and_then(|values| values.get("grpc-status").cloned()));
            assert_eq!(status.unwrap(), if phase.is_some() { "13" } else { "0" });
            if phase.is_some() {
                assert_eq!(count, 0);
                dropped.notified().await;
                let task = child.lock().unwrap().take().unwrap();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                assert_eq!(count, 1);
            }
        }
        drop(client);
        io.abort();
        let _ = io.await;
        stop.send(()).unwrap();
        server.await.unwrap();
    })
    .await
    .expect("panic isolation fixture must be bounded");
}
