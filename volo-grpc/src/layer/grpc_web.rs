use std::task::{Context, Poll};

use futures::{FutureExt, future::BoxFuture};
use http_body_util::BodyExt as _;

use crate::{BoxError, body::BoxBody};

#[derive(Debug, Default, Clone)]
pub struct GrpcWebLayer {
    inner: tonic_web::GrpcWebLayer,
}

impl GrpcWebLayer {
    /// Create a new grpc-web layer.
    pub fn new() -> GrpcWebLayer {
        Self::default()
    }
}

impl<S> tower::Layer<S> for GrpcWebLayer
where
    S: tower::Service<http::Request<BoxBody>, Response = http::Response<BoxBody>>
        + Send
        + Clone
        + 'static,
    S::Error: Into<BoxError> + Send,
    S::Future: Send,
{
    type Service = VoloToTonicService<tonic_web::GrpcWebService<TonicToVoloService<S>>>;

    fn layer(&self, inner: S) -> Self::Service {
        VoloToTonicService::new(self.inner.layer(TonicToVoloService::new(inner)))
    }
}

#[derive(Clone)]
pub struct VoloToTonicService<S> {
    inner: S,
}

impl<S> VoloToTonicService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> tower::Service<http::Request<BoxBody>> for VoloToTonicService<S>
where
    S: tower::Service<
            http::Request<tonic::body::Body>,
            Response = http::Response<tonic::body::Body>,
        > + Send
        + Clone
        + 'static,
    S::Error: Into<BoxError> + Send,
    S::Future: Send,
{
    type Response = http::Response<BoxBody>;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<BoxBody>) -> Self::Future {
        let mut inner = self.inner.clone();
        async move {
            let req = req.map(|body| {
                tonic::body::Body::new(body.map_err(|err| tonic::Status::from_error(Box::new(err))))
            });
            inner.call(req).await.map(|res| {
                res.map(|body| {
                    body.map_err(|err| crate::Status::from_error(Box::new(err)))
                        .boxed_unsync()
                })
            })
        }
        .boxed()
    }
}

#[derive(Clone)]
pub struct TonicToVoloService<S> {
    inner: S,
}

impl<S> TonicToVoloService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> tower::Service<http::Request<tonic::body::Body>> for TonicToVoloService<S>
where
    S: tower::Service<http::Request<BoxBody>, Response = http::Response<BoxBody>>
        + Send
        + Clone
        + 'static,
    S::Error: Into<BoxError> + Send,
    S::Future: Send,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        async move {
            let req = req.map(|body| {
                body.map_err(|err| crate::Status::from_error(Box::new(err)))
                    .boxed_unsync()
            });
            inner.call(req).await.map(|res| {
                res.map(|body| {
                    tonic::body::Body::new(
                        body.map_err(|err| tonic::Status::from_error(Box::new(err))),
                    )
                })
            })
        }
        .boxed()
    }
}
