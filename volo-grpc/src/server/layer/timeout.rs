use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use http::{HeaderMap, HeaderValue};
use metainfo::METAINFO;
use motore::{Service, layer::Layer};
use pin_project::pin_project;
use tokio::time::{self, Sleep};

use crate::{Request, context::ServerContext, status::Status};

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

impl<S, T> Service<ServerContext, Request<T>> for Timeout<S>
where
    S: Service<ServerContext, Request<T>, Error = Status> + Send + Sync,
    T: Send + 'static,
{
    type Response = S::Response;
    type Error = Status;

    async fn call(
        &self,
        _cx: &mut ServerContext,
        req: Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        let client_timeout =
            grpc_timeout_to_duration(req.metadata().headers()).unwrap_or_else(|_| {
                tracing::trace!("Failed to parse grpc-timeout header");
                None
            });

        // insert duration from header (if any) into METAINFO
        if let Some(timeout_val) = client_timeout {
            METAINFO.with(|mi| {
                mi.borrow_mut().insert::<Duration>(timeout_val);
            });
        }

        let sleep = client_timeout.map(time::sleep);
        let inner = self.inner.call(_cx, req);

        ResponseFuture {
            inner,
            sleep: sleep.map(OptionPin::Some).unwrap_or(OptionPin::None),
        }
        .await
    }
}

/// Parse the timeout header in HeaderMap.
///
/// # Return
///
///  Ok(Some(duration)) => if parse success.
///  Ok(None)           => if no success field.
///  Err(&HeaderValue)  => if parse timeout failed or wrong format.
fn grpc_timeout_to_duration(
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
        None => {
            tracing::trace!("grpc-timeout header not found");
            Ok(None)
        }
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
    use crate::metadata::GRPC_TIMEOUT_HEADER;

    // init config in testing
    fn try_set_up(val: Option<&str>) -> Result<Option<Duration>, HeaderValue> {
        let mut hm = HeaderMap::new();
        if let Some(v) = val {
            let hv = HeaderValue::from_str(v).unwrap();
            hm.insert(GRPC_TIMEOUT_HEADER, hv);
        };

        grpc_timeout_to_duration(&hm).map_err(|e| e.clone())
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

#[cfg(test)]
mod tests_insert_and_parse {
    use super::*;

    #[tokio::test]
    async fn test_insert_and_parse_metainfo() {
        use std::time::Duration;

        use http::HeaderValue;
        use metainfo::{METAINFO, MetaInfo};

        let mi = MetaInfo::new();

        METAINFO
            .scope(mi.into(), async {
                // insert a Duration manually
                METAINFO.with(|mi| {
                    mi.borrow_mut().insert::<Duration>(Duration::from_secs(10));
                });

                // verify insertion
                METAINFO.with(|mi| {
                    let mi = mi.borrow();
                    let stored = mi.get::<Duration>().expect("Duration not found");
                    assert_eq!(*stored, Duration::from_secs(10));
                });

                // simulate parsing a grpc-timeout header and inserting
                let mut hm = http::HeaderMap::new();
                let hv = HeaderValue::from_str("7S").unwrap();
                hm.insert(crate::metadata::GRPC_TIMEOUT_HEADER, hv);

                // use parser function
                let parsed = grpc_timeout_to_duration(&hm).expect("Parsing failed");
                assert_eq!(parsed, Some(Duration::from_secs(7)));

                // insert parsed duration
                if let Some(dur) = parsed {
                    METAINFO.with(|mi| {
                        mi.borrow_mut().insert::<Duration>(dur);
                    });
                }

                // check updated duration
                METAINFO.with(|mi| {
                    let mi = mi.borrow();
                    let stored = mi
                        .get::<Duration>()
                        .expect("Duration not found after insert");
                    assert_eq!(*stored, Duration::from_secs(7));
                });
            })
            .await;
    }
}
