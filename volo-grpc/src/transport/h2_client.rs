//! Submission and reset ownership for calls with an owned lifecycle/deadline.
//!
//! The connection task alone opens streams. It polls SETTINGS before checking
//! active slots and serializes the relative budget immediately before HEADERS.
//! No request can sit in h2's pending-open queue with an old timeout value.

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::future;
use http_body::{Body, Frame};
use http_body_util::StreamBody;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use volo::net::Address;

use super::{connect::Connector, h2_body::send_body};
use crate::{
    BoxStream, Status,
    body::{BoxBody, boxed},
    client::Http2Config,
    context::Deadline,
    streaming::StreamContext,
};

type Outgoing = StreamBody<BoxStream<'static, Result<Frame<Bytes>, Status>>>;
type Reply = oneshot::Sender<Result<http::Response<BoxBody>, Status>>;
struct Endpoint {
    connections: tokio::sync::Mutex<Vec<Connection>>,
    queue: Arc<Semaphore>,
}
type Entry = Arc<Endpoint>;

#[derive(Clone)]
pub(super) struct ManagedClient {
    entries: Arc<Mutex<HashMap<Address, Entry>>>,
    connector: Connector,
    config: Http2Config,
}

#[derive(Clone)]
struct Connection {
    requests: mpsc::UnboundedSender<Queued>,
    state: Arc<State>,
}

#[derive(Default)]
struct State {
    closed: AtomicBool,
    active: AtomicUsize,
    pending: AtomicUsize,
    limit: AtomicUsize,
    received: AtomicUsize,
    failure: OnceLock<Status>,
    changed: Notify,
}

struct Queued {
    request: http::Request<Outgoing>,
    reply: Reply,
    _permit: OwnedSemaphorePermit,
    _queued: QueuedSlot,
    abandoned: tokio_util::sync::CancellationToken,
}

impl ManagedClient {
    pub(super) fn new(connector: Connector, config: Http2Config) -> Self {
        Self {
            entries: Arc::default(),
            connector,
            config,
        }
    }

    pub(super) async fn call(
        &self,
        address: Address,
        mut request: http::Request<Outgoing>,
    ) -> Result<http::Response<BoxBody>, Status> {
        // `http+unix` is a private connector key, never a legal HTTP/2
        // :scheme. This transport already dials the explicit Address.
        #[cfg(unix)]
        if matches!(&address, Address::Unix(_)) {
            let mut parts = request.uri().clone().into_parts();
            parts.scheme = Some(http::uri::Scheme::HTTP);
            *request.uri_mut() = http::Uri::from_parts(parts)
                .map_err(|_| Status::internal("invalid Unix request URI"))?;
        }
        let context = request.extensions().get::<StreamContext>().cloned();
        let deadline = request.extensions().get::<Deadline>().copied();
        let call = self.call_inner(address, request);
        if let Some(context) = context {
            tokio::select! { biased;
                status = context.cancelled() => Err(status),
                result = call => result,
            }
        } else if let Some(deadline) = deadline {
            tokio::time::timeout_at(deadline.instant(), call)
                .await
                .map_err(|_| Status::deadline_exceeded("timeout before response headers"))?
        } else {
            call.await
        }
    }

    async fn call_inner(
        &self,
        address: Address,
        request: http::Request<Outgoing>,
    ) -> Result<http::Response<BoxBody>, Status> {
        let entry = {
            let mut entries = self.entries.lock().unwrap();
            entries.retain(|_, entry| match entry.connections.try_lock() {
                Err(_) => true,
                Ok(connections) => {
                    Arc::strong_count(entry) > 1
                        || connections
                            .iter()
                            .any(|connection| !connection.state.closed.load(Ordering::Acquire))
                }
            });
            if !entries.contains_key(&address)
                && entries.len() >= self.config.stream_pool.max_endpoints
            {
                return Err(Status::resource_exhausted("H2 endpoint pool limit reached"));
            }
            entries
                .entry(address.clone())
                .or_insert_with(|| {
                    Arc::new(Endpoint {
                        connections: Default::default(),
                        queue: Arc::new(Semaphore::new(
                            self.config.stream_pool.max_pending_requests,
                        )),
                    })
                })
                .clone()
        };
        let permit = entry
            .queue
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Status::unavailable("H2 submission queue closed"))?;
        let (connection, queued) = {
            let mut connections = entry.connections.lock().await;
            connections.retain(|connection| !connection.state.closed.load(Ordering::Acquire));
            let available = connections
                .iter()
                .enumerate()
                .filter(|(_, connection)| {
                    connection.state.limit.load(Ordering::Acquire) == 0
                        || connection.load() < connection.state.limit.load(Ordering::Acquire)
                })
                .min_by_key(|(_, connection)| connection.load())
                .map(|(index, _)| index);
            let index = if let Some(index) = available {
                index
            } else if connections.len() < self.config.stream_pool.max_connections_per_endpoint {
                connections.push(self.connect(address).await?);
                connections.len() - 1
            } else {
                connections
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, connection)| connection.load())
                    .unwrap()
                    .0
            };
            let connection = connections[index].clone();
            let queued = QueuedSlot::new(connection.state.clone());
            (connection, queued)
        };
        let (reply, response) = oneshot::channel();
        let abandoned = tokio_util::sync::CancellationToken::new();
        let mut wake = WakeOnDrop {
            state: connection.state.clone(),
            abandoned: abandoned.clone(),
            armed: true,
        };
        connection
            .requests
            .send(Queued {
                request,
                reply,
                _permit: permit,
                _queued: queued,
                abandoned,
            })
            .map_err(|_| Status::unavailable("H2 connection closed before submission"))?;
        let result = response
            .await
            .map_err(|_| Status::unavailable("H2 connection closed before response headers"))?;
        wake.armed = false;
        result
    }

    async fn connect(&self, address: Address) -> Result<Connection, Status> {
        let io = motore::service::UnaryService::call(&self.connector, address)
            .await
            .map_err(|error| Status::from_error(Box::new(error)))?;
        let mut builder = h2::client::Builder::new();
        builder
            .initial_window_size(self.config.init_stream_window_size)
            .initial_connection_window_size(self.config.init_connection_window_size)
            .max_frame_size(self.config.max_frame_size)
            .max_header_list_size(self.config.max_header_list_size)
            .max_send_buffer_size(self.config.max_send_buf_size)
            .max_concurrent_reset_streams(self.config.max_concurrent_reset_streams)
            // Wait for the peer's first SETTINGS; its actual limit can be zero.
            .initial_max_send_streams(0);
        let (mut sender, mut io) = builder.handshake::<_, Bytes>(io).await.map_err(h2_error)?;
        let (requests, mut incoming) = mpsc::unbounded_channel::<Queued>();
        let state = Arc::new(State::default());
        let connection = Connection {
            requests,
            state: state.clone(),
        };
        let config = self.config;
        let mut ping = io.ping_pong().expect("one ping owner");
        tokio::spawn(async move {
            let _closed = Closed {
                state: state.clone(),
            };
            let mut ping_sent: Option<tokio::time::Instant> = None;
            let mut pong_received = false;
            let mut next_ping = config
                .http2_keepalive_interval
                .map(|interval| tokio::time::Instant::now() + interval);
            let mut sampled_bytes = 0;
            let mut adaptive_window = config.init_stream_window_size;
            let mut pending = VecDeque::<Queued>::new();
            let mut input_closed = false;
            let mut draining = false;
            loop {
                // Process SETTINGS/GOAWAY before opening anything. All stream
                // creation stays in this task, with no await between this poll,
                // the slot check below, and send_request.
                let closed = future::poll_fn(|task| match Pin::new(&mut io).poll(task) {
                    Poll::Ready(_) => Poll::Ready(true),
                    Poll::Pending => Poll::Ready(false),
                })
                .await;
                if closed {
                    break;
                }
                state
                    .limit
                    .store(io.max_concurrent_send_streams(), Ordering::Release);
                if !draining
                    && future::poll_fn(|task| match sender.poll_ready(task) {
                        Poll::Ready(Err(_)) => Poll::Ready(true),
                        _ => Poll::Ready(false),
                    })
                    .await
                {
                    // GOAWAY retires admission, but already submitted calls keep
                    // their connection until they finish. Never replay HEADERS.
                    draining = true;
                    state.closed.store(true, Ordering::Release);
                    incoming.close();
                    input_closed = true;
                }
                let received = state.received.load(Ordering::Acquire);
                if ping_sent.is_some()
                    && (pong_received
                        || future::poll_fn(|task| match ping.poll_pong(task) {
                            Poll::Ready(Ok(_)) => Poll::Ready(true),
                            Poll::Ready(Err(_)) | Poll::Pending => Poll::Ready(false),
                        })
                        .await)
                {
                    pong_received = false;
                    ping_sent = None;
                    if config.adaptive_window {
                        let observed = received.saturating_sub(sampled_bytes).saturating_mul(2);
                        let grown = (observed.min(16 * 1024 * 1024) as u32).max(adaptive_window);
                        if grown > adaptive_window {
                            adaptive_window = grown;
                            io.set_target_window_size(
                                grown.max(config.init_connection_window_size),
                            );
                            if io.set_initial_window_size(grown).is_err() {
                                break;
                            }
                        }
                        sampled_bytes = received;
                    }
                    next_ping = config
                        .http2_keepalive_interval
                        .map(|interval| tokio::time::Instant::now() + interval);
                }
                let active = state.active.load(Ordering::Acquire);
                let keepalive = (active != 0 || config.http2_keepalive_while_idle)
                    && next_ping.is_some_and(|at| at <= tokio::time::Instant::now());
                let sample_bdp = config.adaptive_window
                    && received.saturating_sub(sampled_bytes)
                        >= (adaptive_window as usize / 2).max(1);
                if ping_sent.is_none() && (keepalive || sample_bdp) {
                    if ping.send_ping(h2::Ping::opaque()).is_err() {
                        break;
                    }
                    ping_sent = Some(tokio::time::Instant::now());
                }
                if ping_sent.is_some_and(|sent| {
                    tokio::time::Instant::now().saturating_duration_since(sent)
                        >= config.http2_keepalive_timeout
                }) {
                    let _ = state.failure.set(Status::unavailable(
                        "H2 keepalive acknowledgement timed out",
                    ));
                    break;
                }
                while let Ok(request) = incoming.try_recv() {
                    pending.push_back(request);
                }
                pending.retain(|request| !request.reply.is_closed());
                if draining {
                    for request in pending.drain(..) {
                        let _ = request.reply.send(Err(Status::unavailable(
                            "H2 connection is draining before submission",
                        )));
                    }
                }
                // Expired entries are removed anywhere in the bounded queue,
                // even while an earlier request is waiting behind a long stream.
                let mut index = 0;
                while index < pending.len() {
                    if pending[index]
                        .request
                        .extensions()
                        .get::<Deadline>()
                        .is_some_and(|deadline| deadline.is_expired())
                    {
                        let request = pending.remove(index).unwrap();
                        let _ = request.reply.send(Err(Status::deadline_exceeded(
                            "timeout before H2 submission",
                        )));
                    } else {
                        index += 1;
                    }
                }
                while !pending.is_empty()
                    && state.active.load(Ordering::Acquire) < io.max_concurrent_send_streams()
                {
                    let Queued {
                        mut request,
                        reply,
                        _permit,
                        _queued,
                        abandoned,
                    } = pending.pop_front().unwrap();
                    if reply.is_closed() {
                        continue;
                    }
                    let deadline = request.extensions().get::<Deadline>().copied();
                    if let Some(deadline) = deadline {
                        if deadline.is_expired() {
                            let _ = reply.send(Err(Status::deadline_exceeded(
                                "timeout before H2 submission",
                            )));
                            continue;
                        }
                        let value = crate::client::layer::timeout::duration_to_grpc_timeout(
                            deadline.remaining(),
                        );
                        request.headers_mut().insert(
                            "grpc-timeout",
                            value.parse().expect("generated timeout is ASCII"),
                        );
                    }
                    let context = request.extensions().get::<StreamContext>().cloned();
                    let (parts, body) = request.into_parts();
                    let end = body.is_end_stream();
                    let ticket = ActiveSlot::new(state.clone());
                    drop(_queued);
                    match sender.send_request(http::Request::from_parts(parts, ()), end) {
                        Ok((response, send)) => {
                            let opened = Opened {
                                response,
                                send: ResetSend {
                                    inner: send,
                                    reset: true,
                                },
                                body,
                                reply: Some(reply),
                                context: context.clone(),
                                deadline,
                                end,
                                abandoned,
                                _ticket: ticket,
                            };
                            if let Some(context) = context {
                                context.spawn_scoped(opened.run());
                            } else {
                                tokio::spawn(opened.run());
                            }
                        }
                        Err(error) => {
                            let _ = reply.send(Err(h2_error(error)));
                        }
                    }
                }
                if input_closed && pending.is_empty() && state.active.load(Ordering::Acquire) == 0 {
                    break;
                }
                let next_deadline = pending
                    .iter()
                    .filter_map(|request| {
                        request
                            .request
                            .extensions()
                            .get::<Deadline>()
                            .map(|deadline| deadline.instant())
                    })
                    .min();
                let wait_deadline = async {
                    if let Some(deadline) = next_deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        future::pending::<()>().await;
                    }
                };
                let active = state.active.load(Ordering::Acquire);
                let wake_ping = match ping_sent {
                    Some(sent) => Some(sent + config.http2_keepalive_timeout),
                    None if active != 0 || config.http2_keepalive_while_idle => next_ping,
                    None => None,
                };
                let wait_ping = async {
                    if let Some(at) = wake_ping {
                        tokio::time::sleep_until(at).await;
                    } else {
                        future::pending::<()>().await;
                    }
                };
                let previous_max = io.max_concurrent_send_streams();
                let connection_event = future::poll_fn(|task| {
                    if Pin::new(&mut io).poll(task).is_ready() {
                        return Poll::Ready(true);
                    }
                    // A SETTINGS update wakes the connection future but does
                    // not complete it. Return to the admission check explicitly.
                    if io.max_concurrent_send_streams() != previous_max
                        || (!draining && matches!(sender.poll_ready(task), Poll::Ready(Err(_))))
                    {
                        Poll::Ready(false)
                    } else {
                        Poll::Pending
                    }
                });
                let pong = future::poll_fn(|task| ping.poll_pong(task));
                tokio::select! {
                    result = pong, if ping_sent.is_some() => match result { Ok(_) => pong_received = true, Err(_) => break },
                    _ = wait_ping => {},
                    closed = connection_event => if closed { break; },
                    request = incoming.recv(), if !input_closed => match request {
                        Some(request) => pending.push_back(request), None => input_closed = true,
                    },
                    _ = state.changed.notified() => {},
                    _ = wait_deadline => {},
                    _ = tokio::time::sleep(config.stream_pool.idle_timeout), if pending.is_empty() && state.active.load(Ordering::Acquire) == 0 => break,
                }
            }
            for request in pending {
                let _ = request.reply.send(Err(Status::unavailable(
                    "H2 connection closed before submission",
                )));
            }
        });
        Ok(connection)
    }
}

struct WakeOnDrop {
    state: Arc<State>,
    abandoned: tokio_util::sync::CancellationToken,
    armed: bool,
}
impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.abandoned.cancel();
        }
        self.state.changed.notify_one();
    }
}
struct Closed {
    state: Arc<State>,
}
impl Drop for Closed {
    fn drop(&mut self) {
        self.state.closed.store(true, Ordering::Release);
        self.state.changed.notify_one();
    }
}
impl Connection {
    fn load(&self) -> usize {
        self.state.active.load(Ordering::Acquire) + self.state.pending.load(Ordering::Acquire)
    }
}
struct QueuedSlot(Arc<State>);
impl QueuedSlot {
    fn new(state: Arc<State>) -> Self {
        state.pending.fetch_add(1, Ordering::AcqRel);
        Self(state)
    }
}
impl Drop for QueuedSlot {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::AcqRel);
    }
}
struct ActiveSlot(Arc<State>);
impl ActiveSlot {
    fn new(state: Arc<State>) -> Self {
        state.active.fetch_add(1, Ordering::AcqRel);
        Self(state)
    }
}
impl Drop for ActiveSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_one();
    }
}
struct ResetSend {
    inner: h2::SendStream<Bytes>,
    reset: bool,
}
impl Drop for ResetSend {
    fn drop(&mut self) {
        if self.reset {
            self.inner.send_reset(h2::Reason::CANCEL);
        }
    }
}

struct Opened {
    response: h2::client::ResponseFuture,
    send: ResetSend,
    body: Outgoing,
    reply: Option<Reply>,
    context: Option<StreamContext>,
    deadline: Option<Deadline>,
    end: bool,
    abandoned: tokio_util::sync::CancellationToken,
    _ticket: ActiveSlot,
}

impl Opened {
    async fn run(mut self) {
        let (frames, receiver) = mpsc::channel(2);
        let failure = Arc::new(OnceLock::new());
        let context = self.context.clone();
        let deadline = self.deadline;
        let mut send_closed = self.end;
        let mut peer_closed_upload = false;
        let result = {
            let connection_state = self._ticket.0.clone();
            let stop = async {
                if let Some(context) = &context {
                    context.cancelled().await
                } else if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline.instant()).await;
                    Status::deadline_exceeded("timeout")
                } else {
                    future::pending::<Status>().await
                }
            };
            let received = async {
                let response = (&mut self.response).await.map_err(h2_error)?;
                let (parts, mut body) = response.into_parts();
                let terminal_header = parts.headers.contains_key("grpc-status");
                let mut end_stream = body.is_end_stream();
                if terminal_header && !end_stream {
                    // A following NO_ERROR can replace h2's EOF state. Probe
                    // without waiting: buffered DATA/trailers or Pending still
                    // make this an invalid early status header. In particular,
                    // data() returning None can mean a failure Trailer is queued.
                    end_stream = matches!(futures::FutureExt::now_or_never(body.data()),
                        Some(Some(Err(error))) if is_h2_no_error_reset(&error));
                }
                let incoming = boxed(QueuedBody {
                    frames: receiver,
                    failure: failure.clone(),
                    done: false,
                    end_stream,
                });
                self.reply
                    .take()
                    .unwrap()
                    .send(Ok(http::Response::from_parts(parts, incoming)))
                    .map_err(|_| Status::cancelled("response future dropped"))?;
                if terminal_header {
                    return Ok(());
                }
                while let Some(data) = body.data().await {
                    let data = match data {
                        Ok(data) => data,
                        Err(error) if is_h2_no_error_reset(&error) => break,
                        Err(error) => return Err(h2_error(error)),
                    };
                    connection_state
                        .received
                        .fetch_add(data.len(), Ordering::Release);
                    connection_state.changed.notify_one();
                    let loan = FlowLoan {
                        flow: body.flow_control().clone(),
                        bytes: data.len(),
                    };
                    frames
                        .send(Received {
                            frame: Frame::data(data),
                            _loan: Some(loan),
                        })
                        .await
                        .map_err(|_| Status::cancelled("response body dropped"))?;
                }
                if let Some(trailer) = body.trailers().await.map_err(h2_error)? {
                    frames
                        .send(Received {
                            frame: Frame::trailers(trailer),
                            _loan: None,
                        })
                        .await
                        .map_err(|_| Status::cancelled("response body dropped"))?;
                }
                Ok::<_, Status>(())
            };
            let sent = send_body(
                &mut self.send.inner,
                &mut self.body,
                &mut send_closed,
                context.as_ref(),
            );
            tokio::pin!(sent, received, stop);
            tokio::select! { biased;
                _ = self.abandoned.cancelled() => Err(Status::cancelled("response future dropped")),
                status = &mut stop => Err(status),
                _ = frames.closed() => Err(Status::cancelled("response body dropped")),
                received = &mut received => received,
                sent = &mut sent => match sent.or_else(|error| {
                    if error.can_finish_from_response() {
                        peer_closed_upload = true;
                        Ok(())
                    } else {
                        Err(error.into_status())
                    }
                }) {
                    Ok(()) => { tokio::select! { biased;
                        _ = self.abandoned.cancelled() => Err(Status::cancelled("response future dropped")),
                        status = &mut stop => Err(status),
                        _ = frames.closed() => Err(Status::cancelled("response body dropped")),
                        received = &mut received => received,
                    } }
                    Err(status) => Err(status),
                }
            }
        };
        if result.is_ok() && (send_closed || peer_closed_upload) {
            self.send.reset = false;
        }
        if let Err(status) = result {
            let status = self._ticket.0.failure.get().cloned().unwrap_or(status);
            let _ = failure.set(status.clone());
            if let Some(context) = &self.context {
                context.finish(status.clone());
            }
            if let Some(reply) = self.reply.take() {
                let _ = reply.send(Err(status));
            }
        }
    }
}

fn h2_error(error: h2::Error) -> Status {
    Status::from_error(Box::new(error))
}

fn is_h2_no_error_reset(error: &h2::Error) -> bool {
    error.is_reset() && error.reason() == Some(h2::Reason::NO_ERROR)
}

struct FlowLoan {
    flow: h2::FlowControl,
    bytes: usize,
}
impl Drop for FlowLoan {
    fn drop(&mut self) {
        let _ = self.flow.release_capacity(self.bytes);
    }
}
struct Received {
    frame: Frame<Bytes>,
    _loan: Option<FlowLoan>,
}
struct QueuedBody {
    frames: mpsc::Receiver<Received>,
    failure: Arc<OnceLock<Status>>,
    done: bool,
    end_stream: bool,
}
impl Body for QueuedBody {
    type Data = Bytes;
    type Error = Status;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        task: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Status>>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.frames.poll_recv(task) {
            Poll::Ready(Some(received)) => Poll::Ready(Some(Ok(received.frame))),
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(self.failure.get().cloned().map(Err))
            }
            Poll::Pending => Poll::Pending,
        }
    }
    fn is_end_stream(&self) -> bool {
        self.done || self.end_stream
    }
}
