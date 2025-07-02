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

use crate::{Request, context::ClientContext, metadata::MetadataValue, status::Status};

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
        let config_timeout = cx.rpc_info.config().rpc_timeout();

        let mi_timeout = METAINFO.with(|m| m.borrow().get::<Duration>().cloned());

        // get the shorter timeout
        let timeout_duration = match (config_timeout, mi_timeout) {
            (None, None) => None,
            (None, Some(t)) | (Some(t), None) => Some(t),
            (Some(t1), Some(t2)) => Some(t1.min(t2)),
        };

        if let Some(timeout) = timeout_duration {
            let header_val = duration_to_grpc_timeout(timeout);
            // Convert to gRPC metadata value and add to outgoing request with header
            if let Ok(meta_val) = MetadataValue::from_str(&header_val) {
                req.metadata_mut()
                    .insert(crate::metadata::GRPC_TIMEOUT_HEADER, meta_val);
            } else {
                tracing::warn!("Invalid grpc-timeout value: {}", header_val);
            }
        }

        let sleep = timeout_duration.map(time::sleep);
        let inner = self.inner.call(cx, req);

        ResponseFuture {
            inner,
            sleep: sleep.map(OptionPin::Some).unwrap_or(OptionPin::None),
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
/// A `String` representing the gRPC timeout format.
fn duration_to_grpc_timeout(duration: Duration) -> String {
    let secs = duration.as_secs();
    let nanos = duration.subsec_nanos();

    if nanos == 0 {
        if secs % 3600 == 0 {
            let hrs = secs / 3600;
            format!("{hrs}H")
        } else if secs % 60 == 0 {
            let mins = secs / 60;
            format!("{mins}M")
        } else {
            format!("{secs}S")
        }
    } else if secs == 0 && nanos % 1_000_000 == 0 {
        let millis = nanos / 1_000_000;
        format!("{millis}m")
    } else if secs == 0 && nanos % 1_000 == 0 {
        let micros = nanos / 1_000;
        format!("{micros}u")
    } else if secs == 0 {
        format!("{nanos}n")
    } else {
        let total_nanos = secs * 1_000_000_000 + nanos as u64;
        format!("{total_nanos}n")
    }
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

        if let Poll::Ready(res) = this.inner.poll(cx) {
            return Poll::Ready(res);
        }

        if let OptionPinProj::Some(sleep) = this.sleep.project() {
            futures_util::ready!(sleep.poll(cx));
            let err = Status::deadline_exceeded("timeout");
            return Poll::Ready(Err(err));
        }

        Poll::Pending
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
}
