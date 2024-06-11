use std::{fmt::Debug, marker::PhantomData};

use http::method::Method;
use motore::{layer::Layer, service::Service};
use volo::net::Address;

use super::{
    handler::{Handler, HandlerService},
    route::MethodRouter,
    IntoResponse,
};
use crate::{
    body::Body, context::ServerContext, request::ServerRequest, response::ServerResponse,
    server::Server,
};

pub fn to_service<H, T, B, E>(handler: H) -> HandlerService<H, T, B, E>
where
    H: Handler<T, B, E>,
{
    handler.into_service()
}

/// Test server which supports many calling methods.
pub struct TestServer<S, B = Body> {
    inner: S,
    _marker: PhantomData<fn(B)>,
}

pub fn empty_address() -> Address {
    use std::net;
    Address::Ip(net::SocketAddr::new(
        net::IpAddr::V4(net::Ipv4Addr::new(127, 0, 0, 1)),
        8000,
    ))
}

pub fn empty_cx() -> ServerContext {
    ServerContext::new(empty_address())
}

pub fn simple_req<S, B>(method: Method, uri: S, body: B) -> ServerRequest<B>
where
    S: AsRef<str>,
{
    ServerRequest::builder()
        .method(method)
        .uri(uri.as_ref())
        .body(body)
        .expect("Failed to build request")
}

impl<B, E> MethodRouter<B, E>
where
    B: Send,
    E: IntoResponse,
{
    pub async fn call_without_cx(&self, req: ServerRequest<B>) -> Result<ServerResponse, E> {
        self.call(&mut ServerContext::new(empty_address()), req)
            .await
    }

    pub async fn call_route<D>(&self, method: Method, data: D) -> ServerResponse
    where
        B: TryFrom<D>,
        B::Error: Debug,
    {
        self.call_without_cx(
            ServerRequest::builder()
                .method(method)
                .uri("/")
                .body(B::try_from(data).expect("Failed to convert data to body"))
                .expect("Failed to build request"),
        )
        .await
        .into_response()
    }
}

impl<S, B> TestServer<S, B>
where
    S: Service<ServerContext, ServerRequest<B>>,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
{
    pub async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<S::Response, S::Error> {
        self.inner.call(cx, req).await
    }

    pub async fn call_without_cx(&self, req: ServerRequest<B>) -> Result<S::Response, S::Error> {
        self.call(&mut ServerContext::new(empty_address()), req)
            .await
    }

    pub async fn call_route<U, D>(&self, method: Method, uri: U, data: D) -> ServerResponse
    where
        U: AsRef<str>,
        B: TryFrom<D>,
        B::Error: Debug,
    {
        self.call_without_cx(
            ServerRequest::builder()
                .method(method)
                .uri(uri.as_ref().to_owned())
                .body(B::try_from(data).expect("Failed to convert data to body"))
                .expect("Failed to build request"),
        )
        .await
        .into_response()
    }
}

impl<S, L> Server<S, L> {
    /// Consume the current server and generate a test server.
    ///
    /// This should be used for unit test only.
    pub fn into_test_server<B>(self) -> TestServer<L::Service, B>
    where
        L: Layer<S>,
        S: Service<ServerContext, ServerRequest<B>>,
    {
        TestServer {
            inner: self.layer.layer(self.service),
            _marker: PhantomData,
        }
    }
}
