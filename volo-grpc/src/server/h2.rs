//! A connection owner keeps HTTP/2 reset/cancel control independent of Body
//! polling and send-window availability. Service/layer/codec routing is shared
//! with the Hyper path used by existing unregistered Protobuf services.

use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::future::{BoxFuture, poll_fn};
use http_body::{Body, Frame};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    sync::{Notify, watch},
};
use tower::{Service, ServiceExt};

use crate::{
    Status,
    body::{BoxBody, boxed},
    message::PayloadKind,
    streaming::StreamContext,
    transport::h2_body::{h2_error, h2_error_with_phase, send_body},
};

#[derive(Clone, Default)]
pub(crate) struct StreamOwner(Arc<Owner>);
#[derive(Default)]
struct Owner {
    context: OnceLock<(StreamContext, PayloadKind)>,
    task: OnceLock<tokio::task::AbortHandle>,
    task_done: AtomicBool,
    terminal: OnceLock<Status>,
    response_policy: parking_lot::Mutex<Option<super::policy::ResponsePolicy>>,
    changed: Notify,
}
impl StreamOwner {
    pub(super) fn set_response_policy(&self, policy: super::policy::ResponsePolicy) {
        *self.0.response_policy.lock() = Some(policy);
    }
    pub(crate) fn attach(&self, cx: StreamContext, payload: PayloadKind) {
        cx.attach_h2_owner();
        let _ = self.0.context.set((cx, payload));
        if let Some(status) = self.0.terminal.get() {
            self.0.context.get().unwrap().0.finish(status.clone());
        }
        self.0.changed.notify_waiters();
    }
    async fn cancelled(&self) -> Status {
        // Once attached, the connection already polls this call independently
        // of handler/Body readiness and H2 send capacity. Keep its deadline here
        // instead of spawning a separate task for every managed request.
        let cx = self.context().await;
        match cx.deadline() {
            Some(deadline) => {
                tokio::select! { biased;
                    status = cx.cancelled() => status,
                    _ = tokio::time::sleep_until(deadline.instant()) => {
                        // Publish before select! drops the suspended call/Body.
                        // Its guards must not replace timeout with local Drop.
                        cx.finish(Status::deadline_exceeded("timeout"));
                        cx.cancelled().await
                    }
                }
            }
            None => cx.cancelled().await,
        }
    }
    async fn context(&self) -> &StreamContext {
        loop {
            if let Some((cx, _)) = self.0.context.get() {
                return cx;
            }
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some((cx, _)) = self.0.context.get() {
                return cx;
            }
            notified.await;
        }
    }
    fn finish(&self, status: Status) {
        let status = self.0.terminal.get_or_init(|| status);
        if let Some((cx, _)) = self.0.context.get() {
            cx.finish(status.clone());
        }
    }
    fn error_response(&self, status: Status) -> http::Response<BoxBody> {
        let context = self.0.context.get();
        let policy = self
            .0
            .response_policy
            .lock()
            .unwrap_or(super::policy::ResponsePolicy {
                payload: context.map(|(_, payload)| *payload).unwrap_or_default(),
                http_status: None,
            });
        super::policy::error_response_with_stream(status, policy, context.map(|(cx, _)| cx))
    }
}
struct RequestTaskGuard(Arc<Owner>);
impl Drop for RequestTaskGuard {
    fn drop(&mut self) {
        self.0.task_done.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}
struct OwnerGuard(StreamOwner);
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        self.0.finish(Status::cancelled("HTTP/2 stream closed"));
    }
}

#[derive(Default)]
struct ConnectionOwners {
    owners: parking_lot::Mutex<Vec<std::sync::Weak<Owner>>>,
    terminal: OnceLock<Status>,
    reclaim: parking_lot::Mutex<Vec<Arc<Owner>>>,
}
impl ConnectionOwners {
    fn register(&self, owner: &StreamOwner) {
        let mut owners = self.owners.lock();
        if owners.len() == owners.capacity() {
            owners.retain(|owner| owner.strong_count() != 0);
        }
        owners.push(Arc::downgrade(&owner.0));
        drop(owners);
        if let Some(status) = self.terminal.get() {
            owner.finish(status.clone());
            self.reclaim.lock().push(owner.0.clone());
        }
    }
    fn contexts(&self) -> Vec<StreamContext> {
        self.owners
            .lock()
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .filter_map(|owner| owner.context.get().map(|(cx, _)| cx.clone()))
            .collect()
    }
    fn finish(&self, status: Status) {
        let status = self.terminal.get_or_init(|| status);
        let owners: Vec<_> = self
            .owners
            .lock()
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        for owner in &owners {
            StreamOwner(owner.clone()).finish(status.clone());
        }
        self.reclaim.lock().extend(owners);
    }
}
impl volo::streaming::NativeConnection for ConnectionOwners {
    fn cancel(&self, error: volo::streaming::RpcError) {
        self.finish(Status::from(error));
        for owner in self.reclaim.lock().iter() {
            if let Some(task) = owner.task.get() {
                task.abort();
            }
        }
    }
    fn active_calls(&self) -> usize {
        self.contexts()
            .iter()
            .filter(|cx| cx.status().is_none())
            .count()
    }
    fn active_tasks(&self) -> usize {
        let mut owners: Vec<_> = self
            .owners
            .lock()
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        owners.extend(self.reclaim.lock().iter().cloned());
        let mut ids = std::collections::HashSet::new();
        owners
            .iter()
            .filter(|owner| ids.insert(Arc::as_ptr(owner)))
            .map(|owner| {
                usize::from(owner.task.get().is_some() && !owner.task_done.load(Ordering::Acquire))
                    + owner.context.get().map_or(0, |(cx, _)| cx.active_tasks())
            })
            .sum()
    }
    fn reclaimed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            loop {
                let owners = std::mem::take(&mut *self.reclaim.lock());
                if owners.is_empty() {
                    break;
                }
                for owner in owners {
                    loop {
                        let changed = owner.changed.notified();
                        if owner.task.get().is_none() || owner.task_done.load(Ordering::Acquire) {
                            break;
                        }
                        changed.await;
                    }
                    if let Some((cx, _)) = owner.context.get() {
                        cx.children_reclaimed().await;
                    }
                }
            }
        })
    }
}
struct ConnectionGuard(Arc<ConnectionOwners>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.finish(if std::thread::panicking() {
            Status::internal("HTTP/2 connection owner panicked")
        } else {
            Status::cancelled("HTTP/2 connection owner dropped")
        });
    }
}

pub(super) async fn serve<S>(
    conn: volo::net::conn::Conn,
    service: S,
    config: super::Http2Config,
    mut shutdown: watch::Receiver<()>,
) where
    S: Service<http::Request<BoxBody>, Response = http::Response<BoxBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Status> + std::error::Error + Send + Sync + 'static,
{
    let (io, h2) = if config.accept_http1 {
        let identified =
            tokio::select! { result = identify(conn) => result, _ = shutdown.changed() => return };
        match identified {
            Ok(value) => value,
            Err(_) => return,
        }
    } else {
        (
            Prefixed {
                inner: conn,
                prefix: Vec::new(),
                offset: 0,
            },
            true,
        )
    };
    if !h2 {
        let mut server =
            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                .http1_only();
        server.http1().keep_alive(true);
        let connection = server.serve_connection(
            hyper_util::rt::TokioIo::new(io),
            hyper::service::service_fn(move |request| {
                let mut service = service.clone();
                async move { service.ready().await?.call(request.map(boxed)).await }
            }),
        );
        tokio::pin!(connection);
        tokio::select! { _ = &mut connection => {}, _ = shutdown.changed() => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }}
        return;
    }
    serve_h2(io, service, config, shutdown).await;
}

/// Serves a connection already classified as HTTP/2 by the shared listener.
/// TLS handshaking precedes detection. All probe bytes are replayed into the H2
/// handshake without splitting the underlying duplex connection.
pub async fn serve_detected<S>(
    io: volo::net::conn::PeekedConn,
    service: S,
    config: super::Http2Config,
    shutdown: watch::Receiver<()>,
) -> std::io::Result<()>
where
    S: Service<http::Request<BoxBody>, Response = http::Response<BoxBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Status> + std::error::Error + Send + Sync + 'static,
{
    config.validate_streamx()?;
    if io.prefix() != b"PRI * HT" || io.negotiated_alpn().is_some_and(|alpn| alpn != b"h2") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "detected HTTP/2 connection has an incompatible prefix or ALPN",
        ));
    }
    let service = super::incoming::IncomingService::new(service, io.info().peer_addr.clone())
        .with_strict_grpc();
    serve_h2(io, service, config, shutdown).await;
    Ok(())
}

pub(super) async fn serve_h2<I, S>(
    io: I,
    service: S,
    config: super::Http2Config,
    mut shutdown: watch::Receiver<()>,
) where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: Service<http::Request<BoxBody>, Response = http::Response<BoxBody>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Status> + std::error::Error + Send + Sync + 'static,
{
    let mut builder = ::h2::server::Builder::new();
    builder
        .initial_window_size(config.init_stream_window_size)
        .initial_connection_window_size(config.init_connection_window_size)
        .max_send_buffer_size(config.max_send_buf_size)
        .max_header_list_size(config.max_header_list_size);
    if let Some(size) = config.max_frame_size {
        builder.max_frame_size(size);
    }
    if let Some(max) = config.max_concurrent_streams {
        builder.max_concurrent_streams(max);
    }
    let connection = tokio::select! { result = builder.handshake::<_, Bytes>(io) => result, _ = shutdown.changed() => return };
    let mut connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            tracing::debug!("gRPC H2 handshake: {error}");
            return;
        }
    };
    let mut requests = tokio::task::JoinSet::new();
    let owners = Arc::new(ConnectionOwners::default());
    volo::streaming::ServerDrain::register(owners.clone());
    let _guard = ConnectionGuard(owners.clone());
    let mut connection_failure = None;
    let traffic = config.adaptive_window.then(|| {
        Arc::new(Traffic {
            sample: crate::transport::bdp::Sample::new(config.init_stream_window_size),
            changed: Notify::new(),
        })
    });
    let mut ping = connection.ping_pong().expect("one ping owner");
    let mut ping_sent = None;
    let mut next_ping = config
        .http2_keepalive_interval
        .map(|interval| tokio::time::Instant::now() + interval);
    let mut sampled = 0;
    let mut window = config.init_stream_window_size;
    let mut draining = false;
    let timer = tokio::time::sleep(std::time::Duration::from_secs(86400));
    tokio::pin!(timer);
    loop {
        let now = tokio::time::Instant::now();
        let bytes = traffic
            .as_ref()
            .map_or(0, |traffic| traffic.sample.received());
        let sample =
            config.adaptive_window && bytes.saturating_sub(sampled) >= (window as usize / 2).max(1);
        if ping_sent.is_none()
            && (sample || (!requests.is_empty() && next_ping.is_some_and(|at| at <= now)))
        {
            if ping.send_ping(::h2::Ping::opaque()).is_err() {
                break;
            }
            sampled = bytes;
            if let Some(traffic) = &traffic {
                traffic.sample.suspend();
            }
            ping_sent = Some(now);
        }
        let wake_at = ping_sent
            .filter(|_| config.http2_keepalive_interval.is_some())
            .map(|sent| sent + config.http2_keepalive_timeout)
            .or_else(|| {
                if requests.is_empty() || ping_sent.is_some() {
                    None
                } else {
                    next_ping
                }
            });
        if let Some(at) = wake_at
            && timer.deadline() != at
        {
            timer.as_mut().reset(at);
        }
        tokio::select! { biased;
            _ = async {
                if let Some(traffic) = &traffic { traffic.changed.notified().await; }
            }, if traffic.is_some() => {},
            _ = shutdown.changed(), if !draining => { draining = true; connection.graceful_shutdown(); },
            result = poll_fn(|task| ping.poll_pong(task)), if ping_sent.is_some() => match result {
                Ok(_) => {
                    ping_sent = None;
                    if let Some(traffic) = &traffic {
                        let bytes = traffic.sample.received();
                        let observed = bytes.saturating_sub(sampled).saturating_mul(2);
                        connection.set_target_window_size((observed.min(16 * 1024 * 1024) as u32).max(config.init_connection_window_size));
                        let target = ((observed / requests.len().max(1)).min(16 * 1024 * 1024) as u32).max(window);
                        if target > window {
                            window = target;
                            if connection.set_initial_window_size(target).is_err() { break; }
                        }
                        sampled = bytes;
                        // Recheck immediately after arming. The next loop also
                        // samples before select; retain a wake if DATA raced it.
                        if traffic.sample.arm(bytes, window).saturating_sub(bytes) >= (window as usize / 2).max(1) {
                            traffic.changed.notify_one();
                        }
                    }
                    next_ping = config.http2_keepalive_interval.map(|interval| tokio::time::Instant::now() + interval);
                }, Err(_) => break,
            },
            _ = &mut timer, if wake_at.is_some() => {
                if ping_sent.is_some() { break; }
            },
            request = connection.accept() => match request {
                Some(Ok((request, response))) => {
                    let body = request.map(|inner| boxed(RequestBody { inner, traffic: traffic.clone(), data_done: false }));
                    let owner = StreamOwner::default();
                    let guard = RequestTaskGuard(owner.0.clone());
                    let task_owner = owner.clone();
                    let service = service.clone();
                    let middleware = volo::streaming::ServerMiddleware::current();
                    let task = requests.spawn(async move {
                        let _guard = guard;
                        volo::streaming::ServerMiddleware::scope_optional(middleware, serve_request(service, body, response, owner)).await;
                    });
                    let _ = task_owner.0.task.set(task.clone());
                    owners.register(&task_owner);
                    if owners.terminal.get().is_some() { task.abort(); }
                },
                Some(Err(error)) => { tracing::debug!("gRPC H2 connection: {error}"); connection_failure = Some(h2_error_with_phase(error, volo::streaming::IoPhase::Read)); break; },
                None => break,
            },
            Some(result) = requests.join_next(), if !requests.is_empty() => {
                if let Err(error) = result { tracing::debug!("gRPC request task: {error}"); }
            },
        }
    }
    // Cancel and join per-request tasks before reclaiming managed children.
    owners.finish(connection_failure.unwrap_or_else(|| {
        Status::from(volo::streaming::RpcError::Transport {
            source: Arc::new(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "HTTP/2 peer closed connection",
            )),
            phase: volo::streaming::IoPhase::Read,
            committed: true,
        })
    }));
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    volo::streaming::NativeConnection::reclaimed(&*owners).await;
}

async fn serve_request<S>(
    service: S,
    request: http::Request<BoxBody>,
    respond: ::h2::server::SendResponse<Bytes>,
    owner: StreamOwner,
) where
    S: Service<http::Request<BoxBody>, Response = http::Response<BoxBody>> + Send,
    S::Future: Send,
    S::Error: Into<Status>,
{
    serve_request_inner(service, request, respond, owner.clone()).await;
    if let Some((cx, _)) = owner.0.context.get() {
        cx.children_reclaimed().await;
    }
}

async fn serve_request_inner<S>(
    mut service: S,
    mut request: http::Request<BoxBody>,
    mut respond: ::h2::server::SendResponse<Bytes>,
    owner: StreamOwner,
) where
    S: Service<http::Request<BoxBody>, Response = http::Response<BoxBody>> + Send,
    S::Future: Send,
    S::Error: Into<Status>,
{
    let _guard = OwnerGuard(owner.clone());
    request.extensions_mut().insert(owner.clone());
    let response = {
        let call = async {
            service
                .ready()
                .await
                .map_err(Into::into)?
                .call(request)
                .await
                .map_err(Into::into)
        };
        tokio::select! { biased;
            _ = poll_fn(|task| respond.poll_reset(task).map(|reset| {
                // select! drops its losing futures before entering a branch.
                // Publish the peer cause while polling, before the suspended
                // call's guard can replace it with a local Drop cancellation.
                owner.finish(h2_error(reset.map_or_else(|error| error, Into::into)));
            })) => return,
            status = owner.cancelled() => Err(status),
            response = catch_call(&owner, call) => response,
        }
    };
    let response = match response {
        Ok(response) => response,
        Err(status) => {
            owner.finish(status.clone());
            owner.error_response(status)
        }
    };
    let (parts, mut body) = response.into_parts();
    let context = parts.extensions.get::<StreamContext>().cloned();
    let shared = context.as_ref().map(StreamContext::shared_context);
    // Outer service layers may wrap the managed body. Scope the complete send
    // operation so those polls retain the same native context and framework locals.
    let sending = async move {
        let mut end = body.is_end_stream();
        let header_status = Status::from_header_map(&parts.headers);
        let mut send = match respond.send_response(http::Response::from_parts(parts, ()), end) {
            Ok(send) => send,
            Err(error) => {
                owner.finish(h2_error_with_phase(error, volo::streaming::IoPhase::Write));
                return;
            }
        };
        if end {
            owner.finish(header_status.unwrap_or_else(|| Status::new(crate::Code::Ok, "")));
            return;
        }
        let result = {
            tokio::select! { biased;
                status = owner.cancelled() => Err(status),
                result = catch_call(&owner, async {
                    send_body(&mut send, &mut body, &mut end, context.as_ref())
                        .await
                        .map_err(|error| match error {
                            crate::transport::h2_body::SendBodyError::Write(error)
                            | crate::transport::h2_body::SendBodyError::Trailers(error) => h2_error_with_phase(error, volo::streaming::IoPhase::Write),
                            error => error.into_status(),
                        })
                }) => result,
            }
        };
        if let Err(status) = result {
            owner.finish(status.clone());
            if let Ok(trailer) = status.to_header_map() {
                let _ = send.send_trailers(trailer);
            } else {
                send.send_reset(::h2::Reason::INTERNAL_ERROR);
            }
        }
    };
    match shared {
        Some(context) => context.scope(sending).await,
        None => sending.await,
    }
}

async fn catch_call<F, T>(owner: &StreamOwner, future: F) -> Result<T, Status>
where
    F: Future<Output = Result<T, Status>>,
{
    tokio::pin!(future);
    poll_fn(
        |task| match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(task))) {
            Ok(poll) => poll,
            Err(_) => {
                let status = Status::internal("gRPC handler or producer panicked");
                // Record failure before dropping the suspended future's guards.
                owner.finish(status.clone());
                Poll::Ready(Err(status))
            }
        },
    )
    .await
}

struct Traffic {
    sample: crate::transport::bdp::Sample,
    changed: Notify,
}

struct RequestBody {
    inner: ::h2::RecvStream,
    traffic: Option<Arc<Traffic>>,
    data_done: bool,
}
impl Body for RequestBody {
    type Data = Bytes;
    type Error = Status;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        task: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Status>>> {
        if !self.data_done {
            match self.inner.poll_data(task) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Some(traffic) = &self.traffic
                        && traffic.sample.record(bytes.len())
                    {
                        traffic.changed.notify_one();
                    }
                    let _ = self.inner.flow_control().release_capacity(bytes.len());
                    return Poll::Ready(Some(Ok(Frame::data(bytes))));
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(h2_error(error)))),
                Poll::Ready(None) => self.data_done = true,
                Poll::Pending => return Poll::Pending,
            }
        }
        self.inner.poll_trailers(task).map(|result| match result {
            Ok(trailers) => trailers.map(|trailer| Ok(Frame::trailers(trailer))),
            Err(error) => Some(Err(h2_error(error))),
        })
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

// Preserve the existing opt-in HTTP/1 acceptance. Only that configuration
// replays the bytes which Hyper's auto connection previously inspected itself.
async fn identify(mut inner: volo::net::conn::Conn) -> std::io::Result<(Prefixed, bool)> {
    const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    let mut prefix = Vec::with_capacity(PREFACE.len());
    let mut h2 = true;
    for expected in PREFACE {
        let byte = inner.read_u8().await?;
        prefix.push(byte);
        if byte != *expected {
            h2 = false;
            break;
        }
    }
    Ok((
        Prefixed {
            inner,
            prefix,
            offset: 0,
        },
        h2,
    ))
}
struct Prefixed {
    inner: volo::net::conn::Conn,
    prefix: Vec<u8>,
    offset: usize,
}
impl AsyncRead for Prefixed {
    fn poll_read(
        mut self: Pin<&mut Self>,
        task: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.offset < self.prefix.len() {
            let count = buf.remaining().min(self.prefix.len() - self.offset);
            buf.put_slice(&self.prefix[self.offset..self.offset + count]);
            self.offset += count;
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.inner).poll_read(task, buf)
        }
    }
}
impl AsyncWrite for Prefixed {
    fn poll_write(
        mut self: Pin<&mut Self>,
        task: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(task, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, task: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(task)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        task: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(task)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        task: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(task, bufs)
    }
}

#[cfg(test)]
mod common_body_scope_tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[tokio::test]
    async fn reclamation_waits_for_owners_registered_while_awaiting() {
        use volo::streaming::NativeConnection;
        let owners = ConnectionOwners::default();
        let first = StreamOwner::default();
        let second = StreamOwner::default();
        let task = tokio::spawn(std::future::pending::<()>());
        first.0.task.set(task.abort_handle()).unwrap();
        second.0.task.set(task.abort_handle()).unwrap();
        owners.register(&first);
        owners.finish(Status::cancelled("shutdown"));
        let reclaim = owners.reclaimed();
        tokio::pin!(reclaim);
        assert!(futures::poll!(&mut reclaim).is_pending());
        owners.register(&second);
        first.0.task_done.store(true, Ordering::Release);
        first.0.changed.notify_waiters();
        let waiting = futures::poll!(&mut reclaim).is_pending();
        second.0.task_done.store(true, Ordering::Release);
        second.0.changed.notify_waiters();
        task.abort();
        assert!(
            waiting,
            "late owner was dropped without waiting for its task"
        );
        reclaim.await;
    }
    use crate::{
        body::Body as Frames,
        message::RpcMode,
        streaming::{CallGuard, lifecycle_body},
    };
    tokio::task_local! { static BODY_TAG: u8; }
    struct Scope;
    impl volo::streaming::StreamScope for Scope {
        fn enter(&self, _: &volo::streaming::StreamContext, operation: &mut dyn FnMut()) {
            BODY_TAG.sync_scope(7, operation);
        }
    }
    struct CheckBody {
        inner: BoxBody,
        id: u64,
        polls: Arc<AtomicUsize>,
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_reclaim_waits_for_task_exit_after_business_drop() {
        use futures::FutureExt;
        use volo::streaming::NativeConnection;

        struct BusinessDrop {
            observed: Option<tokio::sync::oneshot::Sender<()>>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl Drop for BusinessDrop {
            fn drop(&mut self) {
                let _ = self.observed.take().unwrap().send(());
                // Fix the preemption point after the fixture's business Drop
                // marker, before the native HandlerOwner/ChildOwner can exit.
                let _ = self.release.recv();
            }
        }

        for handler in [true, false] {
            let owners = ConnectionOwners::default();
            let owner = StreamOwner::default();
            let cx = StreamContext::new(
                "/Echo/Wait",
                Some(RpcMode::Bidirectional),
                volo::context::Role::Server,
                None,
            );
            owner.attach(cx.clone(), PayloadKind::Thrift);
            owners.register(&owner);
            let (started, running) = tokio::sync::oneshot::channel();
            let (observed, dropped) = tokio::sync::oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let operation = async move {
                let _business = BusinessDrop {
                    observed: Some(observed),
                    release: gate,
                };
                let _ = started.send(());
                std::future::pending::<Result<(), Status>>().await
            };
            if handler {
                cx.start_response_handler(operation).unwrap();
            } else {
                drop(cx.spawn_scoped(operation));
            }
            running.await.unwrap();
            cx.abort(Status::cancelled("peer CANCEL"));
            dropped.await.unwrap();
            assert_eq!(owners.active_calls(), 0);
            assert_eq!(owners.active_tasks(), 1);
            assert!(cx.children_reclaimed().now_or_never().is_none());
            println!(
                "business_drop=true, finished=true, kind={}, calls=0, tasks=1",
                if handler { "handler" } else { "child" }
            );
            release.send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), cx.children_reclaimed())
                .await
                .unwrap();
            assert_eq!(owners.active_tasks(), 0);
            // Both the stream and connection owner remain alive throughout.
            assert_eq!(owners.contexts().len(), 1);
            println!("native_reclaimed=true, connection_owner_alive=true, tasks=0");
        }
    }

    #[tokio::test]
    async fn shutdown_before_context_attachment_keeps_native_cause_and_reclaims_children() {
        use volo::streaming::{CancelCause, NativeConnection, RpcError};
        for registered_before_cancel in [true, false] {
            let owners = ConnectionOwners::default();
            let owner = StreamOwner::default();
            if registered_before_cancel {
                owners.register(&owner);
            }
            owners.cancel(RpcError::Canceled {
                cause: CancelCause::ServerShutdown,
                code: 0,
                message: "shutdown".into(),
                path: Vec::new(),
            });
            if !registered_before_cancel {
                owners.register(&owner);
            }
            let cx = StreamContext::new(
                "/Echo/Call",
                Some(RpcMode::Unary),
                volo::context::Role::Server,
                None,
            );
            let child = cx
                .shared_context()
                .spawn_scoped(std::future::pending::<()>());
            owner.attach(cx.clone(), PayloadKind::Protobuf);
            drop(OwnerGuard(owner));
            assert!(matches!(
                cx.shared_context().cancellation(),
                Some(RpcError::Canceled {
                    cause: CancelCause::ServerShutdown,
                    ..
                })
            ));
            tokio::time::timeout(std::time::Duration::from_secs(1), owners.reclaimed())
                .await
                .unwrap();
            assert!(child.await.is_err());
            assert_eq!(owners.active_tasks(), 0);
        }
    }

    #[tokio::test]
    async fn reset_before_response_preserves_reason_before_call_guard_drop() {
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            for mode in [RpcMode::Unary, RpcMode::ClientStreaming] {
                for reason in [
                    ::h2::Reason::CANCEL,
                    ::h2::Reason::NO_ERROR,
                    ::h2::Reason::INTERNAL_ERROR,
                ] {
                    let (started, mut admitted) = tokio::sync::mpsc::unbounded_channel();
                    let service = tower::service_fn(move |request: http::Request<BoxBody>| {
                        let started = started.clone();
                        async move {
                            let cx = StreamContext::new(
                                "/Echo/Wait",
                                Some(mode),
                                volo::context::Role::Server,
                                None,
                            );
                            request
                                .extensions()
                                .get::<StreamOwner>()
                                .unwrap()
                                .attach(cx.clone(), PayloadKind::Protobuf);
                            let _guard = CallGuard::new(cx.clone());
                            started.send(cx).unwrap();
                            std::future::pending::<Result<http::Response<BoxBody>, Status>>().await
                        }
                    });
                    let (client, server) = tokio::io::duplex(8192);
                    let (shutdown, signal) = watch::channel(());
                    let server = tokio::spawn(serve_h2(
                        server,
                        service,
                        super::super::Http2Config::default(),
                        signal,
                    ));
                    let (mut client, connection) = ::h2::client::handshake(client).await.unwrap();
                    let connection = tokio::spawn(connection);
                    let request = http::Request::builder()
                        .method("POST")
                        .uri("http://localhost/Echo/Wait")
                        .body(())
                        .unwrap();
                    let (response, mut upload) = client
                        .send_request(request, mode == RpcMode::Unary)
                        .unwrap();
                    let cx = admitted.recv().await.unwrap();
                    upload.send_reset(reason);
                    let terminal = cx.cancelled().await;
                    let actual = std::error::Error::source(&terminal)
                        .and_then(|e| e.downcast_ref::<::h2::Error>())
                        .and_then(|e| e.reason());
                    assert_eq!(actual, Some(reason), "{mode:?}: {terminal:?}");
                    drop(response);
                    drop(upload);
                    drop(client);
                    shutdown.send_replace(());
                    server.await.unwrap();
                    connection.abort();
                    let _ = connection.await;
                }
            }
        })
        .await
        .unwrap();
    }
    impl Body for CheckBody {
        type Data = Bytes;
        type Error = Status;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            task: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Status>>> {
            assert_eq!(
                volo::streaming::StreamContext::current().map(|cx| cx.id()),
                Some(self.id)
            );
            assert_eq!(BODY_TAG.try_with(|tag| *tag).ok(), Some(7));
            self.polls.fetch_add(1, Ordering::Relaxed);
            Pin::new(&mut self.inner).poll_frame(task)
        }
        fn is_end_stream(&self) -> bool {
            self.inner.is_end_stream()
        }
    }
    #[tokio::test]
    async fn an_outer_response_body_keeps_the_rpc_and_framework_scopes_after_dispatch() {
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            let polls = Arc::new(AtomicUsize::new(0));
            let observed = polls.clone();
            let service = tower::service_fn(move |request: http::Request<BoxBody>| {
                let polls = observed.clone();
                async move {
                    let cx = StreamContext::new(
                        "/Echo/Call",
                        Some(RpcMode::Unary),
                        volo::context::Role::Server,
                        None,
                    );
                    assert!(cx.shared_context().set_scope(Arc::new(Scope)));
                    request
                        .extensions()
                        .get::<StreamOwner>()
                        .unwrap()
                        .attach(cx.clone(), PayloadKind::Protobuf);
                    let id = cx.id() as u64;
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
                    let frames: Vec<Result<Frame<Bytes>, Status>> = vec![
                        Ok(Frame::data(Bytes::from_static(&[0, 0, 0, 0, 0]))),
                        Ok(Frame::trailers(trailers)),
                    ];
                    let managed = boxed(lifecycle_body::response(
                        boxed(Frames::new(Box::pin(futures::stream::iter(frames)))),
                        CallGuard::new(cx.clone()),
                    ));
                    let mut response = http::Response::new(boxed(CheckBody {
                        inner: managed,
                        id,
                        polls,
                    }));
                    response.headers_mut().insert(
                        "content-type",
                        http::HeaderValue::from_static("application/grpc"),
                    );
                    response.extensions_mut().insert(cx);
                    Ok::<_, Status>(response)
                }
            });
            let (client, server) = tokio::io::duplex(8192);
            let (shutdown, signal) = watch::channel(());
            let server = tokio::spawn(serve_h2(
                server,
                service,
                super::super::Http2Config::default(),
                signal,
            ));
            let (mut client, connection) = ::h2::client::handshake(client).await.unwrap();
            let connection = tokio::spawn(connection);
            let request = http::Request::builder()
                .method(http::Method::POST)
                .uri("http://localhost/Echo/Call")
                .header("content-type", "application/grpc")
                .body(())
                .unwrap();
            let (response, _) = client.send_request(request, true).unwrap();
            let mut body = response.await.unwrap().into_body();
            let mut received = 0;
            while let Some(data) = body.data().await {
                let data = data.unwrap();
                received += data.len();
                body.flow_control().release_capacity(data.len()).unwrap();
            }
            assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
            assert_eq!(received, 5);
            assert!(polls.load(Ordering::Relaxed) >= 2);
            shutdown.send_replace(());
            drop(client);
            server.await.unwrap();
            connection.await.unwrap().unwrap();
        })
        .await
        .unwrap();
    }
}
