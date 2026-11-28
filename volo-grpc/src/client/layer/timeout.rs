use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use metainfo::METAINFO;
use motore::{Service, layer::Layer};
use pin_project::pin_project;
use tokio::time::{self, Sleep};

use crate::{
    Request,
    context::{ClientContext, Deadline},
    status::Status,
    streaming::StreamContext,
};

// Configured timeouts on ordinary Protobuf calls retain the Hyper transport.
#[derive(Clone, Copy)]
pub(crate) struct LegacyTimeout;

/// Timeout middleware that enforces deadlines from ClientContext.
#[derive(Debug, Clone)]
pub struct Timeout<S> {
    inner: S,
}

impl<S> Timeout<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

#[derive(Clone, Default, Copy)]
pub struct TimeoutLayer;

impl TimeoutLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for TimeoutLayer {
    type Service = Timeout<S>;

    fn layer(self, inner: S) -> Self::Service {
        Timeout { inner }
    }
}

impl<S, T> Service<ClientContext, Request<T>> for Timeout<S>
where
    S: Service<ClientContext, Request<T>, Error = Status> + Send + Sync,
    T: Send + 'static,
{
    type Response = S::Response;
    type Error = Status;

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut req: Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        let explicit_deadline = req.extensions().get::<Deadline>().is_some();
        let config_timeout = cx.rpc_info.config().rpc_timeout();

        let (mi_timeout, mi_deadline) = METAINFO
            .try_with(|m| {
                let m = m.borrow();
                (m.get::<Duration>().copied(), m.get::<Deadline>().copied())
            })
            .unwrap_or_default();

        // get the shorter timeout
        let timeout_duration = match (config_timeout, mi_timeout) {
            (None, None) => None,
            (None, Some(t)) | (Some(t), None) => Some(t),
            (Some(t1), Some(t2)) => Some(t1.min(t2)),
        };

        let configured_deadline = timeout_duration
            .and_then(|timeout| time::Instant::now().checked_add(timeout))
            .map(Deadline::new);
        let (within_stream, parent_deadline) = StreamContext::current()
            .map(|cx| (true, cx.deadline()))
            .unwrap_or_default();
        let inherited_deadline = parent_deadline
            .into_iter()
            .chain(mi_deadline)
            .chain(req.extensions().get::<Deadline>().copied())
            .min_by_key(|deadline| deadline.instant());
        let deadline = match (configured_deadline, inherited_deadline) {
            (Some(a), Some(b)) => Some(if a.instant() < b.instant() { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some(deadline) = deadline {
            if deadline.is_expired() {
                return Err(Status::deadline_exceeded("timeout"));
            }
            req.extensions_mut().insert(deadline);
            cx.extensions.insert(deadline);
        }

        let descriptor = req
            .extensions()
            .get::<crate::message::MethodDescriptor>()
            .copied();
        let factory = cx.rpc_info.config().stream_hook.clone();
        let factory = match factory {
            Some(factory) if factory.accepts(descriptor.as_ref())? => Some(factory),
            _ => None,
        };
        // Static Protobuf descriptors are metadata, not a request to replace Hyper.
        if descriptor
            .is_some_and(|method| method.payload != crate::message::PayloadShape::ProtobufMessage)
            || factory.is_some()
            || within_stream
            || explicit_deadline
            || mi_deadline.is_some()
        {
            let stream = StreamContext::new(
                cx.rpc_info.method().clone(),
                descriptor.map(|method| method.mode),
                volo::context::Role::Client,
                deadline,
            );
            let mut guard = crate::streaming::CallGuard::new(stream.clone());
            req.extensions_mut().insert(stream.clone());
            cx.extensions.insert(stream.clone());
            if let Some(descriptor) = descriptor {
                stream.with_extensions(|extensions| extensions.insert(descriptor));
                cx.extensions.insert(descriptor);
            }
            if let Err(status) = stream.install_hook(factory.as_ref()) {
                stream.finish(status.clone());
                return Err(status);
            }
            // This call future owns the opening deadline; Opened::run owns it
            // after submission. No separate per-RPC timer task is needed.
            stream.attach_h2_owner();
            let expire = async {
                match deadline {
                    Some(deadline) => time::sleep_until(deadline.instant()).await,
                    None => std::future::pending().await,
                }
                stream.finish(Status::deadline_exceeded("timeout"));
                Status::deadline_exceeded("timeout")
            };
            let result = stream
                .scope(async {
                    tokio::select! { biased;
                        status = stream.cancelled() => Err(status),
                        status = expire => Err(status),
                        result = self.inner.call(cx, req) => result,
                    }
                })
                .await;
            match result {
                Ok(response) => {
                    guard.disarm();
                    return Ok(response);
                }
                Err(status) => {
                    stream.finish(status.clone());
                    return Err(status);
                }
            }
        }

        req.extensions_mut().insert(LegacyTimeout);
        ResponseFuture {
            inner: self.inner.call(cx, req),
            sleep: deadline
                .map(|deadline| OptionPin::Some(time::sleep_until(deadline.instant())))
                .unwrap_or(OptionPin::None),
        }
        .await
    }
}

/// Converts a `std::time::Duration` to a `String` in gRPC timeout format.
///
/// The gRPC timeout format specifies a duration with a time unit suffix:
/// - `"H"` for hours
/// - `"M"` for minutes
/// - `"S"` for seconds
/// - `"m"` for milliseconds
/// - `"u"` for microseconds
/// - `"n"` for nanoseconds
///
/// This function chooses the largest possible time unit that evenly divides the duration
/// (e.g., 3600 seconds becomes `"1H"`, 60 seconds becomes `"1M"`, 13 milliseconds becomes `"13m"`).
///
/// # Parameters
/// - `duration`: The `Duration` to convert.
///
/// # Returns
/// A header value representing the gRPC timeout format.
pub(crate) fn duration_to_grpc_timeout(duration: Duration) -> http::HeaderValue {
    const MAX: u128 = 99_999_999;
    const UNITS: [(u8, u128); 6] = [
        (b'H', 3_600_000_000_000),
        (b'M', 60_000_000_000),
        (b'S', 1_000_000_000),
        (b'm', 1_000_000),
        (b'u', 1_000),
        (b'n', 1),
    ];
    let nanos = duration.as_nanos();
    for (unit, scale) in UNITS {
        if nanos.is_multiple_of(scale) && nanos / scale <= MAX {
            return timeout_header(nanos / scale, unit);
        }
    }
    for (unit, scale) in UNITS.into_iter().rev() {
        let rounded = nanos.div_ceil(scale);
        if rounded <= MAX {
            return timeout_header(rounded, unit);
        }
    }
    timeout_header(MAX, b'H')
}

// Eight decimal digits plus a unit, without an intermediate heap String.
fn timeout_header(mut value: u128, unit: u8) -> http::HeaderValue {
    let mut buffer = [0; 9];
    buffer[8] = unit;
    let mut start = 8;
    loop {
        start -= 1;
        buffer[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    http::HeaderValue::from_bytes(&buffer[start..]).expect("decimal timeout is ASCII")
}

#[pin_project]
pub struct ResponseFuture<F> {
    #[pin]
    inner: F,
    #[pin]
    sleep: OptionPin<Sleep>,
}

#[pin_project(project = OptionPinProj)]
pub enum OptionPin<T> {
    Some(#[pin] T),
    None,
}

impl<F, R> Future for ResponseFuture<F>
where
    F: Future<Output = Result<R, Status>>,
{
    type Output = Result<R, Status>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if let OptionPinProj::Some(sleep) = this.sleep.project()
            && (sleep.deadline() <= time::Instant::now() || sleep.poll(cx).is_ready())
        {
            return Poll::Ready(Err(Status::deadline_exceeded("timeout")));
        }
        this.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    #[test]
    fn test_hours() {
        let converted_duration = duration_to_grpc_timeout(Duration::from_secs(3 * 3600));
        assert_eq!("3H", converted_duration);
    }

    #[test]
    fn test_minutes() {
        let converted_duration = duration_to_grpc_timeout(Duration::from_secs(60));
        assert_eq!("1M", converted_duration);
    }

    #[test]
    fn test_seconds() {
        let converted_duration = duration_to_grpc_timeout(Duration::from_secs(42));
        assert_eq!("42S", converted_duration);
    }

    #[test]
    fn test_milliseconds() {
        let converted_duration = duration_to_grpc_timeout(Duration::from_millis(13));
        assert_eq!("13m", converted_duration);
    }

    #[test]
    fn test_microseconds() {
        let converted_duration = duration_to_grpc_timeout(Duration::from_micros(2));
        assert_eq!("2u", converted_duration);
    }

    #[test]
    fn test_nanoseconds() {
        let converted_duration = duration_to_grpc_timeout(Duration::from_nanos(82));
        assert_eq!("82n", converted_duration);
    }

    #[tokio::test]
    async fn explicit_request_deadline_is_preserved_and_expiry_skips_inner_service() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use volo::context::{Endpoint, Role, RpcInfo};

        use crate::context::{Config, Deadline};

        struct Probe(Arc<AtomicUsize>);
        impl Service<ClientContext, Request<()>> for Probe {
            type Response = Deadline;
            type Error = Status;
            async fn call(
                &self,
                _: &mut ClientContext,
                req: Request<()>,
            ) -> Result<Deadline, Status> {
                self.0.fetch_add(1, Ordering::Relaxed);
                let deadline = *req.extensions().get::<Deadline>().unwrap();
                let stream = req
                    .extensions()
                    .get::<crate::streaming::StreamContext>()
                    .unwrap();
                assert_eq!(stream.deadline().unwrap().instant(), deadline.instant());
                stream.finish(Status::ok(""));
                Ok(deadline)
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let service = Timeout::new(Probe(calls.clone()));
        let mut cx = ClientContext::new(RpcInfo::new(
            Role::Client,
            "/deadline/Call".into(),
            Endpoint::new("caller".into()),
            Endpoint::new("callee".into()),
            Config::default(),
        ));
        let deadline = Deadline::new(time::Instant::now() + Duration::from_secs(1));
        let mut request = Request::new(());
        request.extensions_mut().insert(deadline);
        assert_eq!(
            service.call(&mut cx, request).await.unwrap().instant(),
            deadline.instant()
        );
        let mut expired = Request::new(());
        expired
            .extensions_mut()
            .insert(Deadline::new(time::Instant::now() - Duration::from_secs(1)));
        assert_eq!(
            service.call(&mut cx, expired).await.unwrap_err().code(),
            crate::Code::DeadlineExceeded
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn timeout_rounding_is_bounded_and_never_overflows_the_wire_field() {
        assert_eq!(
            duration_to_grpc_timeout(Duration::from_nanos(100_000_001)),
            "100001u"
        );
        assert_eq!(duration_to_grpc_timeout(Duration::ZERO), "0H");
        assert_eq!(duration_to_grpc_timeout(Duration::MAX), "99999999H");
    }

    #[tokio::test]
    async fn protobuf_descriptor_alone_does_not_select_managed_transport() {
        use crate::message::{MethodDescriptor, PayloadShape, RpcMode};
        struct Probe;
        impl Service<ClientContext, Request<()>> for Probe {
            type Response = (bool, bool);
            type Error = Status;
            async fn call(
                &self,
                _: &mut ClientContext,
                req: Request<()>,
            ) -> Result<Self::Response, Status> {
                assert!(req.extensions().get::<MethodDescriptor>().is_some());
                Ok((
                    req.extensions().get::<StreamContext>().is_some(),
                    req.extensions().get::<LegacyTimeout>().is_some(),
                ))
            }
        }
        for mode in [
            RpcMode::Unary,
            RpcMode::ClientStreaming,
            RpcMode::ServerStreaming,
            RpcMode::Bidirectional,
        ] {
            let mut request = Request::new(());
            request.extensions_mut().insert(MethodDescriptor {
                name: "Call",
                mode,
                payload: PayloadShape::ProtobufMessage,
            });
            let result = Timeout::new(Probe)
                .call(&mut ClientContext::default(), request)
                .await
                .unwrap();
            assert_eq!(result, (false, true));
        }
    }
}
