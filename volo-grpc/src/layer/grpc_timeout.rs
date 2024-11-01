use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use http::{HeaderMap, HeaderValue};
use motore::{layer::Layer, Service};
use pin_project::pin_project;
use tokio::time::Sleep;

use crate::status::Status;

/// A [`Service`] that parse 'grpc-timeout' header and choose the minimum value compared with
/// server's configured timeout to set the request.
#[derive(Debug, Clone)]
pub struct GrpcTimeout<S> {
    inner: S,
    server_timeout: Option<Duration>,
}

impl<S> GrpcTimeout<S> {
    pub fn new(inner: S, server_timeout: Option<Duration>) -> Self {
        Self {
            inner,
            server_timeout,
        }
    }
}

#[derive(Clone)]
pub struct GrpcTimeoutLayer {
    timeout: Option<Duration>,
}

impl GrpcTimeoutLayer {
    pub fn new(timeout: Option<Duration>) -> Self {
        Self { timeout }
    }
}

impl<S> Layer<S> for GrpcTimeoutLayer {
    type Service = GrpcTimeout<S>;

    fn layer(self, inner: S) -> Self::Service {
        GrpcTimeout::new(inner, self.timeout)
    }
}

/// Parse the timeout header in HeaderMap.
///
/// # Return
///
///  Ok(Some(duration)) => if parse success.
///  Ok(None)           => if no success field.
///  Err(&HeaderValue)  => if parse timeout failed or wrong format.
fn try_parse_client_timeout(
    headers: &HeaderMap<HeaderValue>,
) -> Result<Option<Duration>, &HeaderValue> {
    const SECONDS_HOUR: u64 = 60 * 60;
    const SECONDS_MINUTE: u64 = 60;

    match headers.get(crate::metadata::GRPC_TIMEOUT_HEADER) {
        Some(val) => {
            // parse the value and unit
            let (timeout_value, timeout_unit) = val
                .to_str()
                .map_err(|_| val)
                .and_then(|s| if s.is_empty() { Err(val) } else { Ok(s) })?
                .split_at(val.len() - 1);
            let timeout_value = timeout_value.parse::<u64>().map_err(|_| val)?;
            // match the unit with Hour | Minute | Second | Milliseconds | Microsecond | Nanosecond
            let duration = match timeout_unit {
                "H" => Duration::from_secs(timeout_value * SECONDS_HOUR),
                "M" => Duration::from_secs(timeout_value * SECONDS_MINUTE),
                "S" => Duration::from_secs(timeout_value),
                "m" => Duration::from_millis(timeout_value),
                "u" => Duration::from_micros(timeout_value),
                "n" => Duration::from_nanos(timeout_value),
                _ => return Err(val),
            };
            Ok(Some(duration))
        }
        None => Ok(None),
    }
}

impl<Cx, S, ReqBody> Service<Cx, hyper::Request<ReqBody>> for GrpcTimeout<S>
where
    Cx: Send,
    S: Service<Cx, hyper::Request<ReqBody>, Error = Status> + Send + Sync,
    ReqBody: 'static + Send,
{
    type Response = S::Response;
    type Error = Status;

    async fn call(
        &self,
        cx: &mut Cx,
        req: hyper::Request<ReqBody>,
    ) -> Result<Self::Response, Self::Error> {
        // parse the client_timeout
        let client_timeout = try_parse_client_timeout(req.headers()).unwrap_or_else(|_| {
            tracing::trace!("[VOLO] error parsing grpc-timeout header");
            None
        });

        // get the shorter timeout
        let timeout_duration = match (client_timeout, self.server_timeout) {
            (None, None) => None,
            (None, Some(t)) | (Some(t), None) => Some(t),
            (Some(t1), Some(t2)) => Some(t1.min(t2)),
        };

        // map it into pinned tokio::time::Sleep
        let pined_sleep = match timeout_duration {
            Some(duration) => OptionPin::Some(tokio::time::sleep(duration)),
            None => OptionPin::None,
        };

        // return the future, the executor can poll then
        ResponseFuture {
            inner: self.inner.call(cx, req),
            sleep: pined_sleep,
        }
        .await
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

/// Basically, this is almost the same with implementation of [`tower::timeout`][tower_timeout].
/// The only difference here is the sleep is optional, so we use a OptionPin instead.
///
/// [tower_timeout]: https://docs.rs/tower/0.4.13/tower/timeout/index.html
impl<F, R> Future for ResponseFuture<F>
where
    F: Future<Output = Result<R, Status>>,
{
    type Output = Result<R, Status>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if let Poll::Ready(result) = this.inner.poll(cx) {
            return Poll::Ready(result);
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
    use crate::metadata::GRPC_TIMEOUT_HEADER;

    // init config in testing
    fn try_set_up(val: Option<&str>) -> Result<Option<Duration>, HeaderValue> {
        let mut hm = HeaderMap::new();
        if let Some(v) = val {
            let hv = HeaderValue::from_str(v).unwrap();
            hm.insert(GRPC_TIMEOUT_HEADER, hv);
        };

        try_parse_client_timeout(&hm).map_err(|e| e.clone())
    }

    #[test]
    fn test_hours() {
        let parsed_duration = try_set_up(Some("3H")).unwrap().unwrap();
        assert_eq!(Duration::from_secs(3 * 60 * 60), parsed_duration);
    }

    #[test]
    fn test_minutes() {
        let parsed_duration = try_set_up(Some("1M")).unwrap().unwrap();
        assert_eq!(Duration::from_secs(60), parsed_duration);
    }

    #[test]
    fn test_seconds() {
        let parsed_duration = try_set_up(Some("42S")).unwrap().unwrap();
        assert_eq!(Duration::from_secs(42), parsed_duration);
    }

    #[test]
    fn test_milliseconds() {
        let parsed_duration = try_set_up(Some("13m")).unwrap().unwrap();
        assert_eq!(Duration::from_millis(13), parsed_duration);
    }

    #[test]
    fn test_microseconds() {
        let parsed_duration = try_set_up(Some("2u")).unwrap().unwrap();
        assert_eq!(Duration::from_micros(2), parsed_duration);
    }

    #[test]
    fn test_nanoseconds() {
        let parsed_duration = try_set_up(Some("82n")).unwrap().unwrap();
        assert_eq!(Duration::from_nanos(82), parsed_duration);
    }

    #[test]
    fn test_corner_cases() {
        // error postfix
        let r = HeaderValue::from_str("82f").unwrap();
        assert_eq!(try_set_up(Some("82f")), Err(r));

        // error digit
        let r = HeaderValue::from_str("abcH").unwrap();
        assert_eq!(try_set_up(Some("abcH")), Err(r));
    }
}
