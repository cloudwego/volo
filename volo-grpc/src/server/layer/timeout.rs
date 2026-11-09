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

use crate::{
    Request,
    context::{Deadline, ServerContext},
    status::Status,
};

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
        cx: &mut ServerContext,
        req: Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        let deadline = req.extensions().get::<Deadline>().copied().or_else(|| {
            let timeout =
                grpc_timeout_to_duration(req.metadata().headers()).unwrap_or_else(|_| {
                    tracing::trace!("Failed to parse grpc-timeout header");
                    None
                })?;
            time::Instant::now().checked_add(timeout).map(Deadline::new)
        });
        if deadline.is_some_and(Deadline::is_expired) {
            return Err(Status::deadline_exceeded("timeout"));
        }

        // Keep the relative value for existing middleware. New callers use the
        // absolute deadline so time spent in middleware is deducted only once.
        if let Some(deadline) = deadline {
            METAINFO.with(|mi| {
                let mut mi = mi.borrow_mut();
                mi.insert::<Duration>(deadline.remaining());
                mi.insert::<Deadline>(deadline);
            });
        }

        let sleep = deadline.map(|deadline| time::sleep_until(deadline.instant()));
        let inner = self.inner.call(cx, req);

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
pub(crate) fn grpc_timeout_to_duration(
    headers: &HeaderMap<HeaderValue>,
) -> Result<Option<Duration>, &HeaderValue> {
    const SECONDS_HOUR: u64 = 60 * 60;
    const SECONDS_MINUTE: u64 = 60;

    match headers.get(crate::metadata::GRPC_TIMEOUT_HEADER) {
        Some(val) => {
            // parse the value and unit
            let text = val.to_str().map_err(|_| val)?;
            // The wire grammar permits 1..=8 ASCII digits and one ASCII unit.
            if !(2..=9).contains(&text.len()) || !text.is_ascii() {
                return Err(val);
            }
            let (timeout_value, timeout_unit) = text.split_at(text.len() - 1);
            if !timeout_value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(val);
            }
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

        if let OptionPinProj::Some(sleep) = this.sleep.project() {
            // Check the instant as well as the timer: a newly created zero-time
            // Sleep need not have received its timer-wheel notification yet.
            if sleep.deadline() <= time::Instant::now() || sleep.poll(cx).is_ready() {
                return Poll::Ready(Err(Status::deadline_exceeded("timeout")));
            }
        }

        this.inner.poll(cx)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::metadata::GRPC_TIMEOUT_HEADER;

    #[tokio::test]
    async fn expired_deadline_does_not_poll_handler() {
        let called = std::cell::Cell::new(false);
        let response = ResponseFuture {
            inner: async {
                called.set(true);
                Ok(())
            },
            sleep: OptionPin::Some(time::sleep(Duration::ZERO)),
        }
        .await;
        assert_eq!(response.unwrap_err().code(), crate::Code::DeadlineExceeded);
        assert!(!called.get());
    }

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

        // Peer-controlled values cannot overflow multiplication or split a
        // multibyte character at an invalid UTF-8 boundary.
        for value in [
            "",
            "1",
            "1é",
            "+1S",
            "-1S",
            "100000000S",
            "18446744073709551615H",
        ] {
            assert!(try_set_up(Some(value)).is_err(), "{value}");
        }
        assert_eq!(try_set_up(Some("0n")), Ok(Some(Duration::ZERO)));
        assert_eq!(try_set_up(None), Ok(None));
        assert_eq!(
            try_set_up(Some("99999999H")),
            Ok(Some(Duration::from_secs(99999999 * 3600)))
        );
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
