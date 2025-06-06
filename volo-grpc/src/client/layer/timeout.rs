use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use motore::{layer::Layer, Service};
use pin_project::pin_project;
use tokio::time::{self, Sleep};

use crate::{context::ClientContext, status::Status};

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

impl<S, Req> Service<ClientContext, Req> for Timeout<S>
where
    S: Service<ClientContext, Req, Error = Status> + Send + Sync,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = Status;

    async fn call(&self, cx: &mut ClientContext, req: Req) -> Result<Self::Response, Self::Error> {
        let timeout_duration = cx.rpc_info.config().rpc_timeout();

        let sleep = timeout_duration.map(time::sleep);
        let inner = self.inner.call(cx, req);

        ResponseFuture {
            inner,
            sleep: sleep.map(OptionPin::Some).unwrap_or(OptionPin::None),
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
            if sleep.poll(cx).is_ready() {
                return Poll::Ready(Err(Status::deadline_exceeded("timeout")));
            }
        }

        Poll::Pending
    }
}
