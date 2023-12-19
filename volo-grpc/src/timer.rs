// The `TokioTimer` and `TokioSleep` are copied from `hyper-util`.
//
// Since these are very important, but the new version of `hyper-util` containing these
// has not been released yet, I copied it temporarily.
//
// This file will be removed once the `hyper-util` 0.1.2 released, and
// `hyper_util::rt::TokioTimer` will be used directly.
//
// Refer: https://github.com/hyperium/hyper-util/pull/73

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use hyper::rt::{Sleep, Timer};
use pin_project::pin_project;

/// A Timer that uses the tokio runtime.
#[non_exhaustive]
#[derive(Default, Clone, Debug)]
pub struct TokioTimer;

// Use TokioSleep to get tokio::time::Sleep to implement Unpin.
// see https://docs.rs/tokio/latest/tokio/time/struct.Sleep.html
#[pin_project]
#[derive(Debug)]
struct TokioSleep {
    #[pin]
    inner: tokio::time::Sleep,
}

// ==== impl TokioTimer =====

impl Timer for TokioTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
        Box::pin(TokioSleep {
            inner: tokio::time::sleep(duration),
        })
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>> {
        Box::pin(TokioSleep {
            inner: tokio::time::sleep_until(deadline.into()),
        })
    }

    fn reset(&self, sleep: &mut Pin<Box<dyn Sleep>>, new_deadline: Instant) {
        if let Some(sleep) = sleep.as_mut().downcast_mut_pin::<TokioSleep>() {
            sleep.reset(new_deadline)
        }
    }
}

impl TokioTimer {
    /// Create a new TokioTimer
    pub fn new() -> Self {
        Self {}
    }
}

impl Future for TokioSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx)
    }
}

impl Sleep for TokioSleep {}

impl TokioSleep {
    fn reset(self: Pin<&mut Self>, deadline: Instant) {
        self.project().inner.as_mut().reset(deadline.into());
    }
}
