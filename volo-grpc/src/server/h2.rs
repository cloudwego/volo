//! A connection owner keeps HTTP/2 reset/cancel control independent of Body
//! polling and send-window availability. Service/layer/codec routing is shared
//! with the Hyper path used by existing unregistered Protobuf services.

use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{
    StreamExt,
    future::{BoxFuture, poll_fn},
    stream::FuturesUnordered,
};
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
    transport::h2_body::{h2_error, send_body},
};

#[derive(Clone, Default)]
pub(crate) struct StreamOwner(Arc<Owner>);
#[derive(Default)]
struct Owner {
    context: OnceLock<(StreamContext, PayloadKind)>,
    changed: Notify,
}
impl StreamOwner {
    pub(crate) fn attach(&self, cx: StreamContext, payload: PayloadKind) {
        let _ = self.0.context.set((cx, payload));
        self.0.changed.notify_waiters();
    }
    async fn cancelled(&self) -> Status {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some((cx, _)) = self.0.context.get() {
                return cx.cancelled().await;
            }
            notified.await;
        }
    }
    fn finish(&self, status: Status) {
        if let Some((cx, _)) = self.0.context.get() {
            cx.finish(status);
        }
    }
    fn content_type(&self) -> &'static str {
        match self.0.context.get() {
            Some((_, PayloadKind::Thrift)) => "application/grpc+thrift",
            _ => "application/grpc",
        }
    }
}
struct OwnerGuard(StreamOwner);
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        self.0.finish(Status::cancelled("HTTP/2 stream closed"));
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
    let mut requests = FuturesUnordered::<BoxFuture<'static, ()>>::new();
    let received = Arc::new(AtomicUsize::new(0));
    let traffic_changed = Arc::new(Notify::new());
    let mut ping = connection.ping_pong().expect("one ping owner");
    let mut ping_sent = None;
    let mut next_ping = config
        .http2_keepalive_interval
        .map(|interval| tokio::time::Instant::now() + interval);
    let mut sampled = 0;
    let mut window = config.init_stream_window_size;
    let mut draining = false;
    loop {
        let now = tokio::time::Instant::now();
        let bytes = received.load(Ordering::Acquire);
        let sample =
            config.adaptive_window && bytes.saturating_sub(sampled) >= (window as usize / 2).max(1);
        if ping_sent.is_none()
            && (sample || (!requests.is_empty() && next_ping.is_some_and(|at| at <= now)))
        {
            if ping.send_ping(::h2::Ping::opaque()).is_err() {
                break;
            }
            ping_sent = Some(now);
        }
        let wake_at = ping_sent
            .map(|sent| sent + config.http2_keepalive_timeout)
            .or_else(|| if requests.is_empty() { None } else { next_ping });
        let timer = async {
            if let Some(at) = wake_at {
                tokio::time::sleep_until(at).await;
            } else {
                futures::future::pending::<()>().await;
            }
        };
        tokio::select! { biased;
            _ = traffic_changed.notified(), if config.adaptive_window => {},
            _ = shutdown.changed(), if !draining => { draining = true; connection.graceful_shutdown(); },
            result = poll_fn(|task| ping.poll_pong(task)), if ping_sent.is_some() => match result {
                Ok(_) => {
                    ping_sent = None;
                    if config.adaptive_window {
                        let bytes = received.load(Ordering::Acquire);
                        let target = (bytes.saturating_sub(sampled).saturating_mul(2).min(16 * 1024 * 1024) as u32).max(window);
                        if target > window {
                            window = target;
                            connection.set_target_window_size(target.max(config.init_connection_window_size));
                            if connection.set_initial_window_size(target).is_err() { break; }
                        }
                        sampled = bytes;
                    }
                    next_ping = config.http2_keepalive_interval.map(|interval| tokio::time::Instant::now() + interval);
                }, Err(_) => break,
            },
            _ = timer => {
                if ping_sent.is_some() { break; }
            },
            Some(()) = requests.next(), if !requests.is_empty() => {},
            request = connection.accept() => match request {
                Some(Ok((request, response))) => {
                    let body = request.map(|inner| boxed(RequestBody { inner, received: received.clone(), traffic_changed: traffic_changed.clone(), adaptive: config.adaptive_window, data_done: false }));
                    requests.push(Box::pin(serve_request(service.clone(), body, response)));
                },
                Some(Err(error)) => { tracing::debug!("gRPC H2 connection: {error}"); break; },
                None => break,
            },
        }
    }
    // These are futures owned by the connection, so Drop releases all handlers
    // and producers synchronously, rather than leaving detached stream tasks.
    drop(requests);
}

async fn serve_request<S>(
    mut service: S,
    mut request: http::Request<BoxBody>,
    mut respond: ::h2::server::SendResponse<Bytes>,
) where
    S: Service<http::Request<BoxBody>, Response = http::Response<BoxBody>> + Send,
    S::Future: Send,
    S::Error: Into<Status>,
{
    let owner = StreamOwner::default();
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
            _ = poll_fn(|task| respond.poll_reset(task)) => return,
            status = owner.cancelled() => Err(status),
            response = catch_call(&owner, call) => response,
        }
    };
    let response = match response {
        Ok(response) => response,
        Err(status) => {
            owner.finish(status.clone());
            let mut response = http::Response::new(crate::body::empty_body());
            *response.headers_mut() = status.to_header_map().unwrap_or_default();
            response.headers_mut().insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static(owner.content_type()),
            );
            response
        }
    };
    let (parts, mut body) = response.into_parts();
    let context = parts.extensions.get::<StreamContext>().cloned();
    let mut end = body.is_end_stream();
    let header_status = Status::from_header_map(&parts.headers);
    let mut send = match respond.send_response(http::Response::from_parts(parts, ()), end) {
        Ok(send) => send,
        Err(error) => {
            owner.finish(h2_error(error));
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
                    .map_err(|error| error.into_status())
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

struct RequestBody {
    inner: ::h2::RecvStream,
    received: Arc<AtomicUsize>,
    traffic_changed: Arc<Notify>,
    adaptive: bool,
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
                    self.received.fetch_add(bytes.len(), Ordering::Release);
                    if self.adaptive {
                        self.traffic_changed.notify_one();
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
