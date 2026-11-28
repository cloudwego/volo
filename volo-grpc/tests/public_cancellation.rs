//! G0 transport prototype: verify real Hyper/H2 future and Body Drop signals.
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures::{Stream, future};
use http_body::Frame;
use http_body_util::BodyExt;
use motore::Service;
use tokio::sync::Notify;
use volo::net::incoming::DefaultIncoming;
use volo_grpc::{
    Request, Response, Status,
    body::{Body, BoxBody, boxed},
    context::ServerContext,
    server::{NamedService, Server},
};

#[derive(Clone, Copy)]
enum Phase {
    Handler,
    Producer,
}

#[derive(Clone, Copy)]
enum Termination {
    Reset,
    Disconnect,
    Drain,
    RunFutureDrop,
}

type ManagedTask = tokio::task::JoinHandle<Result<(), Status>>;
type ManagedSlot = Arc<Mutex<Option<ManagedTask>>>;

#[derive(Clone)]
struct Probe {
    phase: Phase,
    consume_request: bool,
    started: Arc<Notify>,
    dropped: Arc<Notify>,
    managed: Option<ManagedSlot>,
}

struct DropSignal(Arc<Notify>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

struct PendingProducer {
    _signal: DropSignal,
    _request: Option<BoxBody>,
    context: Option<volo_grpc::streaming::StreamContext>,
    sent: bool,
}
impl Stream for PendingProducer {
    type Item = Result<Frame<Bytes>, Status>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(context) = &this.context {
            assert_eq!(
                volo_grpc::streaming::StreamContext::current().unwrap().id(),
                context.id()
            );
            use metainfo::Forward;
            assert_eq!(
                metainfo::METAINFO
                    .with(|slot| slot.borrow().get_transient("phase"))
                    .as_deref(),
                Some("producer")
            );
        }
        if !this.sent {
            this.sent = true;
            return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"\0\0\0\0\0")))));
        }
        Poll::Pending
    }
}

impl NamedService for Probe {
    const NAME: &'static str = "public.CancelProbe";
    fn descriptor() -> Option<volo_grpc::message::ServiceDescriptor> {
        use volo_grpc::message::{
            MethodDescriptor, PayloadKind, PayloadShape, RpcMode, ServiceDescriptor,
        };
        Some(ServiceDescriptor {
            payload: PayloadKind::Protobuf,
            methods: &[MethodDescriptor {
                name: "Call",
                mode: RpcMode::Unary,
                payload: PayloadShape::ProtobufMessage,
            }],
        })
    }
    fn stream_hook_factory(&self) -> Option<Arc<dyn volo_grpc::streaming::StreamHookFactory>> {
        self.managed.as_ref().map(|_| {
            Arc::new(|_: &volo_grpc::streaming::StreamContext| volo_grpc::streaming::NoopHooks)
                as Arc<dyn volo_grpc::streaming::StreamHookFactory>
        })
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
        let signal = DropSignal(self.dropped.clone());
        let context = request
            .extensions()
            .get::<volo_grpc::streaming::StreamContext>()
            .cloned();
        if request.metadata().get("x-local-cancel").is_some() {
            let cx = context.as_ref().unwrap().clone();
            context.as_ref().unwrap().spawn_scoped(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                cx.cancel();
            });
        }
        if let Some(tasks) = &self.managed {
            let context = context
                .as_ref()
                .expect("registered factory installs context before the service");
            assert_eq!(context.mode(), Some(volo_grpc::message::RpcMode::Unary));
            let (ready, started) = tokio::sync::oneshot::channel();
            *tasks.lock().unwrap() = Some(context.spawn_scoped(async {
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
            }));
            started.await.unwrap();
            use metainfo::Forward;
            metainfo::METAINFO.with(|slot| slot.borrow_mut().set_transient("phase", "producer"));
        }
        let mut request = request.into_inner();
        if self.consume_request {
            while let Some(frame) = request.frame().await {
                frame?;
            }
        }
        self.started.notify_one();
        match self.phase {
            Phase::Handler => {
                future::pending::<()>().await;
                drop((signal, request));
                unreachable!()
            }
            Phase::Producer => Ok(Response::new(boxed(Body::new(Box::pin(PendingProducer {
                _signal: signal,
                _request: Some(request),
                context,
                sent: false,
            }))))),
        }
    }
}

async fn cancellation_probe(
    phase: Phase,
    consume_request: bool,
    termination: Termination,
    managed: bool,
    zero_window: bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(Notify::new());
        let managed = managed.then(|| Arc::new(Mutex::new(None)));
        let server = Server::new()
            .graceful_shutdown_timeout(Duration::from_millis(50))
            .add_service(Probe {
                phase,
                consume_request,
                started: started.clone(),
                dropped: dropped.clone(),
                managed: managed.clone(),
            });
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let mut stop_tx = Some(stop_tx);
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
        let (mut client, connection) = h2::client::Builder::new()
            .initial_window_size(if zero_window { 0 } else { 65535 })
            .handshake(socket)
            .await
            .unwrap();
        let connection_task = tokio::spawn(connection);
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("http://{address}/public.CancelProbe/Call"))
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (response, mut send) = client.send_request(request, false).unwrap();
        send.send_data(Bytes::from_static(b"\0\0\0\0\0"), consume_request)
            .unwrap();
        started.notified().await;
        // Keep the response future alive during the disconnect case. Dropping
        // it here would itself send RST and could falsely "prove" EOF handling.
        let mut response_future = Some(response);
        let mut response = match phase {
            Phase::Producer => Some(response_future.take().unwrap().await.unwrap()),
            Phase::Handler => None,
        };
        if !zero_window && let Some(response) = &mut response {
            let body = response.body_mut();
            let data = body.data().await.unwrap().unwrap();
            assert_eq!(data.as_ref(), b"\0\0\0\0\0");
            body.flow_control().release_capacity(data.len()).unwrap();
            // The producer now remains pending without a final Trailer.
        }
        match termination {
            Termination::Drain => stop_tx.take().unwrap().send(()).unwrap(),
            Termination::Disconnect => connection_task.abort(),
            Termination::RunFutureDrop => server_task.abort(),
            Termination::Reset => send.send_reset(h2::Reason::CANCEL),
        }
        tokio::time::timeout(Duration::from_secs(1), dropped.notified())
            .await
            .expect("actual handler/producer owner must be dropped after RST/EOF");
        if let Some(tasks) = managed {
            let task = tasks.lock().unwrap().take().unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), task)
                    .await
                    .unwrap()
                    .unwrap_err()
                    .is_cancelled()
            );
        }
        drop(response);
        drop(response_future);
        drop(client);
        connection_task.abort();
        let _ = connection_task.await;
        if matches!(termination, Termination::RunFutureDrop) {
            assert!(server_task.await.unwrap_err().is_cancelled());
        } else {
            if let Some(stop_tx) = stop_tx {
                stop_tx.send(()).unwrap();
            }
            server_task.await.unwrap();
        }
    })
    .await
    .expect("cancellation probe must finish within its budget");
}

#[tokio::test]
async fn reset_after_request_end_stream_drops_pending_handler() {
    cancellation_probe(Phase::Handler, true, Termination::Reset, false, false).await;
}

#[tokio::test]
async fn disconnect_after_request_end_stream_drops_pending_handler() {
    cancellation_probe(Phase::Handler, true, Termination::Disconnect, false, false).await;
}

#[tokio::test]
async fn reset_after_response_headers_drops_pending_producer() {
    cancellation_probe(Phase::Producer, true, Termination::Reset, false, false).await;
}

#[tokio::test]
async fn reset_is_observed_when_the_application_never_polls_request_body() {
    cancellation_probe(Phase::Producer, false, Termination::Reset, false, false).await;
}

#[tokio::test]
async fn drain_deadline_reclaims_a_handler_pending_after_request_end_stream() {
    cancellation_probe(Phase::Handler, true, Termination::Drain, false, false).await;
}

#[tokio::test]
async fn drain_deadline_reclaims_a_pending_producer_after_headers() {
    cancellation_probe(Phase::Producer, true, Termination::Drain, false, false).await;
}

#[tokio::test]
async fn managed_children_stop_after_reset_without_further_application_receives() {
    cancellation_probe(Phase::Handler, true, Termination::Reset, true, false).await;
}
#[tokio::test]
async fn managed_producer_and_child_stop_after_headers_and_disconnect() {
    cancellation_probe(Phase::Producer, false, Termination::Disconnect, true, false).await;
}
#[tokio::test]
async fn managed_children_are_reclaimed_on_drain_expiration() {
    cancellation_probe(Phase::Producer, true, Termination::Drain, true, false).await;
}
#[tokio::test]
async fn reset_with_zero_send_window_still_reclaims_producer_and_children() {
    cancellation_probe(Phase::Producer, true, Termination::Reset, true, true).await;
}

#[tokio::test]
async fn local_cancel_and_deadline_reclaim_producer_with_zero_send_window() {
    for deadline in [true, false] {
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let dropped = Arc::new(Notify::new());
            let managed = Arc::new(Mutex::new(None));
            let probe = Probe {
                phase: Phase::Producer,
                consume_request: true,
                started: Arc::default(),
                dropped: dropped.clone(),
                managed: Some(managed.clone()),
            };
            let (stop, stopped) = tokio::sync::oneshot::channel();
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
            let (mut client, io) = h2::client::Builder::new()
                .initial_window_size(0)
                .handshake(socket)
                .await
                .unwrap();
            let io = tokio::spawn(io);
            let header = if deadline {
                "grpc-timeout"
            } else {
                "x-local-cancel"
            };
            let request = http::Request::builder()
                .method("POST")
                .uri(format!("http://{address}/public.CancelProbe/Call"))
                .header("content-type", "application/grpc")
                .header(header, "80m")
                .body(())
                .unwrap();
            let (response, mut send) = client.send_request(request, false).unwrap();
            send.send_data(Bytes::from_static(b"\0\0\0\0\0"), true)
                .unwrap();
            let mut body = response.await.unwrap().into_body();
            dropped.notified().await;
            assert!(body.data().await.is_none());
            let trailer = body.trailers().await.unwrap().unwrap();
            let expected = if deadline { "4" } else { "1" };
            assert_eq!(trailer["grpc-status"], expected);
            let child = managed.lock().unwrap().take().unwrap();
            assert!(child.await.unwrap_err().is_cancelled());
            drop((send, body, client));
            io.abort();
            let _ = io.await;
            stop.send(()).unwrap();
            server.await.unwrap();
        })
        .await
        .expect("local termination must not wait for WINDOW_UPDATE");
    }
}

#[tokio::test]
async fn dropping_server_run_reclaims_live_handler_and_managed_children() {
    cancellation_probe(
        Phase::Handler,
        true,
        Termination::RunFutureDrop,
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn dropping_server_run_reclaims_live_producer_and_managed_children() {
    cancellation_probe(
        Phase::Producer,
        true,
        Termination::RunFutureDrop,
        true,
        false,
    )
    .await;
}
