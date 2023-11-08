use http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};

use crate::HttpContext;

pub trait Layer<S> {
    type Service: motore::Service<HttpContext, Request<Incoming>>;

    fn layer(self, inner: S) -> Self::Service;
}

pub trait LayerExt {
    fn method(
        self,
        method: Method,
    ) -> FilterLayer<Box<dyn Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode>>>
    where
        Self: Sized,
    {
        self.filter(Box::new(
            move |cx: &mut HttpContext, _: &Request<Incoming>| {
                if cx.method == method {
                    Ok(())
                } else {
                    Err(StatusCode::METHOD_NOT_ALLOWED)
                }
            },
        ))
    }

    fn filter<F>(self, f: F) -> FilterLayer<F>
    where
        Self: Sized,
        F: Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode>,
    {
        FilterLayer { f }
    }
}

pub struct FilterLayer<F> {
    f: F,
}

impl<S, F> Layer<S> for FilterLayer<F>
where
    S: motore::Service<HttpContext, Request<Incoming>, Response = Response<Full<Bytes>>>
        + Send
        + Sync
        + 'static,
    F: Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode> + Send + Sync,
{
    type Service = Filter<S, F>;

    fn layer(self, inner: S) -> Self::Service {
        Filter {
            service: inner,
            f: self.f,
        }
    }
}

pub struct Filter<S, F> {
    service: S,
    f: F,
}

impl<S, F> motore::Service<HttpContext, Request<Incoming>> for Filter<S, F>
where
    S: motore::Service<HttpContext, Request<Incoming>, Response = Response<Full<Bytes>>>
        + Send
        + Sync
        + 'static,
    F: Fn(&mut HttpContext, &Request<Incoming>) -> Result<(), StatusCode> + Send + Sync,
{
    type Response = S::Response;

    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Request<Incoming>,
    ) -> Result<Self::Response, Self::Error> {
        if let Err(status) = (self.f)(cx, &req) {
            return Ok(Response::builder()
                .status(status)
                .body(Full::new(Bytes::new()))
                .unwrap());
        }
        self.service.call(cx, req).await
    }
}
