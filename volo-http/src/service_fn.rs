use std::{convert::Infallible, fmt, future::Future};

use motore::service::Service;

use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

/// Returns a new [`ServiceFn`] with the given closure.
///
/// This lets you build a [`Service`] from an async function that returns a [`Result`].
pub fn service_fn<F>(f: F) -> ServiceFn<F> {
    ServiceFn { f }
}

/// A [`Service`] implemented by a closure. See the docs for [`service_fn`] for more details.
#[derive(Clone)]
pub struct ServiceFn<F> {
    f: F,
}

impl<F> Service<ServerContext, ServerRequest> for ServiceFn<F>
where
    F: for<'r> Callback<'r>,
{
    type Response = ServerResponse;
    type Error = Infallible;

    fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut ServerContext,
        req: ServerRequest,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> {
        (self.f).call(cx, req)
    }
}

impl<F> fmt::Debug for ServiceFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceFn")
            .field("f", &format_args!("{}", std::any::type_name::<F>()))
            .finish()
    }
}

/// [`Service`] for binding lifetime to return value while using closure.
/// This is just a temporary workaround for lifetime issues.
///
/// Related issue: https://github.com/rust-lang/rust/issues/70263.
/// Related RFC: https://github.com/rust-lang/rfcs/pull/3216.
pub trait Callback<'r> {
    type Future: Future<Output = Result<ServerResponse, Infallible>> + Send + 'r;

    fn call(&self, cx: &'r mut ServerContext, req: ServerRequest) -> Self::Future;
}

impl<'r, F, Fut> Callback<'r> for F
where
    F: Fn(&'r mut ServerContext, ServerRequest) -> Fut,
    Fut: Future<Output = Result<ServerResponse, Infallible>> + Send + 'r,
{
    type Future = Fut;

    fn call(&self, cx: &'r mut ServerContext, req: ServerRequest) -> Self::Future {
        self(cx, req)
    }
}
