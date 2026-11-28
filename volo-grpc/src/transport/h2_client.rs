//! Submission and reset ownership for calls with an owned lifecycle/deadline.
//!
//! The connection task alone opens streams. It polls SETTINGS before checking
//! active slots and serializes the relative budget immediately before HEADERS.
//! No request can sit in h2's pending-open queue with an old timeout value.

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::future;
use http_body::{Body, Frame};
use http_body_util::StreamBody;
use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashMap;
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
    authority: http::uri::Authority,
    connections: Mutex<Vec<Connection>>,
    queue: Arc<Semaphore>,
    connecting: AtomicUsize,
    changed: Notify,
}
struct Connecting<'a>(&'a Endpoint);
impl Drop for Connecting<'_> {
    fn drop(&mut self) {
        self.0.connecting.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}
type Entry = Arc<Endpoint>;

#[derive(Clone)]
pub(super) struct ManagedClient {
    entries: Arc<RwLock<FxHashMap<Address, Entry>>>,
    connector: Connector,
    config: Http2Config,
    validation: Result<(), Status>,
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
    sample: super::bdp::Sample,
    adaptive: bool,
    failure: OnceLock<Status>,
    changed: Notify,
}

impl State {
    fn record_received(&self, len: usize) {
        if self.adaptive && self.sample.record(len) {
            self.changed.notify_one();
        }
    }
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
            validation: config.stream_pool.validate(),
            config,
        }
    }

    pub(super) async fn call(
        &self,
        address: Address,
        request: http::Request<Outgoing>,
    ) -> Result<http::Response<BoxBody>, Status> {
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
        mut request: http::Request<Outgoing>,
    ) -> Result<http::Response<BoxBody>, Status> {
        self.validation.as_ref().map_err(Clone::clone)?;
        let cached = self.entries.read().get(&address).cloned();
        let entry =
            match cached {
                Some(entry) => entry,
                None => {
                    let mut entries = self.entries.write();
                    if !entries.contains_key(&address)
                        && entries.len() >= self.config.stream_pool.max_endpoints
                    {
                        entries.retain(|_, entry| {
                            Arc::strong_count(entry) > 1
                                || entry.connections.lock().iter().any(|connection| {
                                    !connection.state.closed.load(Ordering::Acquire)
                                })
                        });
                    }
                    if !entries.contains_key(&address)
                        && entries.len() >= self.config.stream_pool.max_endpoints
                    {
                        return Err(Status::resource_exhausted("H2 endpoint pool limit reached"));
                    }
                    entries
                        .entry(address.clone())
                        .or_insert_with(|| {
                            Arc::new(Endpoint {
                                authority: super::client::build_uri(address.clone(), "/")
                                    .authority()
                                    .expect("endpoint authority")
                                    .clone(),
                                connections: Default::default(),
                                connecting: AtomicUsize::new(0),
                                changed: Notify::new(),
                                queue: Arc::new(Semaphore::new(
                                    self.config.stream_pool.max_pending_requests,
                                )),
                            })
                        })
                        .clone()
                }
            };
        *request.uri_mut() = http::Uri::builder()
            .scheme(
                request
                    .uri()
                    .scheme()
                    .cloned()
                    .unwrap_or(http::uri::Scheme::HTTP),
            )
            .authority(
                request
                    .uri()
                    .authority()
                    .unwrap_or(&entry.authority)
                    .clone(),
            )
            .path_and_query(
                request
                    .uri()
                    .path_and_query()
                    .cloned()
                    .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/")),
            )
            .build()
            .map_err(|_| Status::internal("invalid H2 request URI"))?;
        let permit = entry
            .queue
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Status::unavailable("H2 submission queue closed"))?;
        let (connection, queued) = loop {
            let changed = entry.changed.notified();
            let (chosen, connecting) = {
                let mut connections = entry.connections.lock();
                connections.retain(|connection| !connection.state.closed.load(Ordering::Acquire));
                let available = connections
                    .iter()
                    .filter(|connection| {
                        connection.state.limit.load(Ordering::Acquire) == 0
                            || connection.load() < connection.state.limit.load(Ordering::Acquire)
                    })
                    .min_by_key(|connection| connection.load());
                if let Some(connection) = available {
                    (
                        Some((
                            connection.clone(),
                            QueuedSlot::new(connection.state.clone()),
                        )),
                        None,
                    )
                } else if connections.len() + entry.connecting.load(Ordering::Acquire)
                    < self.config.stream_pool.max_connections_per_endpoint
                {
                    entry.connecting.fetch_add(1, Ordering::AcqRel);
                    (None, Some(Connecting(&entry)))
                } else {
                    (
                        connections
                            .iter()
                            .min_by_key(|connection| connection.load())
                            .map(|connection| {
                                (
                                    connection.clone(),
                                    QueuedSlot::new(connection.state.clone()),
                                )
                            }),
                        None,
                    )
                }
            };
            if let Some(chosen) = chosen {
                break chosen;
            }
            if let Some(connecting) = connecting {
                let connection = self.connect(address.clone()).await?;
                let queued = QueuedSlot::new(connection.state.clone());
                entry.connections.lock().push(connection.clone());
                drop(connecting);
                break (connection, queued);
            }
            changed.await;
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
        let state = Arc::new(State {
            adaptive: self.config.adaptive_window,
            sample: super::bdp::Sample::new(self.config.init_stream_window_size),
            ..Default::default()
        });
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
            let mut idle_since = None;
            let mut draining = false;
            let timer = tokio::time::sleep(std::time::Duration::ZERO);
            tokio::pin!(timer);
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
                let mut received = state.sample.received();
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
                        received = state.sample.received();
                        let observed = received.saturating_sub(sampled_bytes).saturating_mul(2);
                        let streams = state.active.load(Ordering::Acquire).max(1);
                        io.set_target_window_size(
                            (observed.min(16 * 1024 * 1024) as u32)
                                .max(config.init_connection_window_size),
                        );
                        let grown = ((observed / streams).min(16 * 1024 * 1024) as u32)
                            .max(adaptive_window);
                        if grown > adaptive_window {
                            adaptive_window = grown;
                            if io.set_initial_window_size(grown).is_err() {
                                break;
                            }
                        }
                        sampled_bytes = received;
                        received = state.sample.arm(received, adaptive_window);
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
                    sampled_bytes = received;
                    state.sample.suspend();
                    ping_sent = Some(tokio::time::Instant::now());
                }
                if config.http2_keepalive_interval.is_some()
                    && ping_sent.is_some_and(|sent| {
                        tokio::time::Instant::now().saturating_duration_since(sent)
                            >= config.http2_keepalive_timeout
                    })
                {
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
                        request.headers_mut().insert("grpc-timeout", value);
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
                let active = state.active.load(Ordering::Acquire);
                let wake_ping = match ping_sent {
                    Some(sent) if config.http2_keepalive_interval.is_some() => {
                        Some(sent + config.http2_keepalive_timeout)
                    }
                    Some(_) => None,
                    None if active != 0 || config.http2_keepalive_while_idle => next_ping,
                    None => None,
                };
                let idle_deadline = if pending.is_empty() && active == 0 {
                    Some(
                        *idle_since.get_or_insert_with(tokio::time::Instant::now)
                            + config.stream_pool.idle_timeout,
                    )
                } else {
                    idle_since = None;
                    None
                };
                if idle_deadline.is_some_and(|at| at <= tokio::time::Instant::now()) {
                    break;
                }
                let next_timer = next_deadline
                    .into_iter()
                    .chain(wake_ping)
                    .chain(idle_deadline)
                    .min();
                if let Some(at) = next_timer
                    && timer.deadline() != at
                {
                    timer.as_mut().reset(at);
                }
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
                    _ = &mut timer, if next_timer.is_some() => {},
                    closed = connection_event => if closed { break; },
                    request = incoming.recv(), if !input_closed => match request {
                        Some(request) => pending.push_back(request), None => input_closed = true,
                    },
                    _ = state.changed.notified() => {},
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
        // One Tokio channel block amortizes bursty DATA handoffs. Outstanding
        // FlowLoans still bound buffered bytes by this stream's H2 receive window.
        let (frames, receiver) = mpsc::channel(32);
        let failure = Arc::new(OnceLock::new());
        let context = self.context.clone();
        let deadline = self.deadline;
        let mut send_closed = self.end;
        let mut peer_closed_upload = false;
        let result = {
            let connection_state = self._ticket.0.clone();
            let stop = async {
                let cancel = async {
                    match &context {
                        Some(context) => context.cancelled().await,
                        None => future::pending::<Status>().await,
                    }
                };
                let expire = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline.instant()).await,
                        None => future::pending().await,
                    }
                    Status::deadline_exceeded("timeout")
                };
                tokio::select! { biased; status = cancel => status, status = expire => status }
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
                    connection_state.record_received(data.len());
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

#[cfg(test)]
mod review_tests {
    use std::time::Duration;

    use http_body_util::BodyExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::task::AbortOnDropHandle;

    use super::*;

    #[tokio::test]
    async fn managed_requests_preserve_explicit_scheme_and_fill_relative_authority() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let _server = AbortOnDropHandle::new(tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(socket).await.unwrap();
                while let Some(Ok((request, mut response))) = connection.accept().await {
                    let response_head = http::Response::builder()
                        .header("observed-uri", request.uri().to_string())
                        .body(())
                        .unwrap();
                    response.send_response(response_head, true).unwrap();
                }
            }));
            let client = ManagedClient::new(Connector::default(), Http2Config::default());
            for uri in [
                "https://custom.example/S/M?key=value",
                "http://custom.example/S/M",
                "/S/M",
            ] {
                let body: BoxStream<'static, Result<Frame<Bytes>, Status>> =
                    Box::pin(futures::stream::empty());
                let response = client
                    .call(
                        Address::Ip(address),
                        http::Request::builder()
                            .uri(uri)
                            .body(StreamBody::new(body))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let expected = if uri.starts_with('/') {
                    format!("http://{address}{uri}")
                } else {
                    uri.to_owned()
                };
                assert_eq!(response.headers()["observed-uri"], expected);
            }
        })
        .await
        .unwrap();
    }

    async fn discard_pongs(
        mut reader: tokio::net::tcp::OwnedReadHalf,
        mut writer: tokio::net::tcp::OwnedWriteHalf,
        observed: Arc<Notify>,
    ) -> std::io::Result<()> {
        loop {
            let mut header = [0; 9];
            reader.read_exact(&mut header).await?;
            let len = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
            let mut payload = vec![0; len];
            reader.read_exact(&mut payload).await?;
            if header[3] == 6 && header[4] & 1 != 0 {
                // Suppress PING ACK, preserving SETTINGS/DATA and the socket.
                observed.notify_one();
                continue;
            }
            writer.write_all(&header).await?;
            writer.write_all(&payload).await?;
        }
    }

    #[tokio::test]
    async fn bdp_ping_without_keepalive_does_not_retire_an_unacknowledged_connection() {
        tokio::time::timeout(Duration::from_secs(3), async {
            let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let server_address = server.local_addr().unwrap();
            let _server = AbortOnDropHandle::new(tokio::spawn(async move {
                let (socket, _) = server.accept().await.unwrap();
                let mut connection = h2::server::handshake(socket).await.unwrap();
                let mut responses = Vec::new();
                while let Some(Ok((_, mut response))) = connection.accept().await {
                    let mut send = response
                        .send_response(http::Response::new(()), false)
                        .unwrap();
                    send.send_data(Bytes::from(vec![0; 64]), false).unwrap();
                    responses.push(send);
                }
            }));
            let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = Address::Ip(proxy.local_addr().unwrap());
            let discarded_pong = Arc::new(Notify::new());
            let observed = discarded_pong.clone();
            let _proxy = AbortOnDropHandle::new(tokio::spawn(async move {
                let (client, _) = proxy.accept().await.unwrap();
                let server = tokio::net::TcpStream::connect(server_address)
                    .await
                    .unwrap();
                let (mut client_read, client_write) = client.into_split();
                let (server_read, mut server_write) = server.into_split();
                let requests = tokio::io::copy(&mut client_read, &mut server_write);
                let responses = discard_pongs(server_read, client_write, observed);
                tokio::select! { _ = requests => {}, _ = responses => {} }
            }));
            let config = Http2Config {
                adaptive_window: true,
                init_stream_window_size: 64,
                http2_keepalive_interval: None,
                http2_keepalive_timeout: Duration::from_millis(20),
                ..Default::default()
            };
            let client = ManagedClient::new(Connector::default(), config);
            let body: BoxStream<'static, Result<Frame<Bytes>, Status>> =
                Box::pin(futures::stream::empty());
            let response = client
                .call(
                    address.clone(),
                    http::Request::builder()
                        .method(http::Method::POST)
                        .uri("http://localhost/test/Call")
                        .body(StreamBody::new(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            let mut body = response.into_body();
            assert_eq!(
                body.frame()
                    .await
                    .unwrap()
                    .unwrap()
                    .data_ref()
                    .unwrap()
                    .len(),
                64
            );
            discarded_pong.notified().await;
            let endpoint = client.entries.read().get(&address).unwrap().clone();
            let connection = endpoint.connections.lock()[0].state.clone();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!connection.closed.load(Ordering::Acquire));
            assert!(connection.failure.get().is_none());
        })
        .await
        .expect("BDP-only PING must not enable the keepalive timeout");
    }
}
