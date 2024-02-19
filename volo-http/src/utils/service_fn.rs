use std::{fmt, future::Future};

use motore::service::Service;

/// Returns a new [`ServiceFn`] with the given closure.
///
/// This lets you build a [`Service`] from an async function that returns a [`Result`].
pub fn service_fn<F>(f: F) -> ServiceFn<F> {
    ServiceFn { f }
}

/// A [`Service`] implemented by a closure. See the docs for [`service_fn`] for more details.
#[derive(Copy, Clone)]
pub struct ServiceFn<F> {
    f: F,
}

impl<Cx, F, Request, R, E> Service<Cx, Request> for ServiceFn<F>
where
    F: for<'r> Callback<'r, Cx, Request, Response = R, Error = E>,
    Request: 'static,
    R: 'static,
    E: 'static,
{
    type Response = R;
    type Error = E;

    fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut Cx,
        req: Request,
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
pub trait Callback<'r, Cx, Request> {
    type Response;
    type Error;
    type Future: Future<Output = Result<Self::Response, Self::Error>> + Send + 'r;

    fn call(&self, cx: &'r mut Cx, req: Request) -> Self::Future;
}

impl<'r, F, Fut, Cx, Request, R, E> Callback<'r, Cx, Request> for F
where
    F: Fn(&'r mut Cx, Request) -> Fut,
    Fut: Future<Output = Result<R, E>> + Send + 'r,
    Cx: 'r,
{
    type Response = R;
    type Error = E;
    type Future = Fut;

    fn call(&self, cx: &'r mut Cx, req: Request) -> Self::Future {
        self(cx, req)
    }
}
