//! Test utilities for server of Volo-HTTP.

use std::{fmt::Debug, marker::PhantomData};

use http::method::Method;
use motore::{layer::Layer, service::Service};

use super::{
    IntoResponse,
    handler::{Handler, HandlerService},
    route::method_router::MethodRouter,
};
use crate::{
    body::Body, context::ServerContext, request::Request, response::Response, server::Server,
    utils::test_helpers::mock_address,
};

/// Wrap a [`Handler`] into a [`HandlerService`].
///
/// Since [`Handler`] is not exposed, the [`Handler::into_service`] cannot be called outside of
/// Volo-HTTP.
///
/// For testing purposes, this function wraps [`Handler::into_service`] and is made public.
pub fn to_service<H, T, B, E>(handler: H) -> HandlerService<H, T, B, E>
where
    H: Handler<T, B, E>,
{
    Handler::into_service(handler)
}

/// Test server which supports many calling methods.
///
/// Supported methods:
///
/// - [`TestServer::call`], which is a naive service call
/// - [`TestServer::call_without_cx`], which is a naive service call without passing `cx`
/// - [`TestServer::call_route`], which is a simple call with given method, uri and data
pub struct TestServer<S, B = Body> {
    inner: S,
    _marker: PhantomData<fn(B)>,
}

/// Create an empty [`ServerContext`].
///
/// The context has only caller address.
pub fn empty_cx() -> ServerContext {
    ServerContext::new(mock_address())
}

impl<B, E> MethodRouter<B, E>
where
    B: Send,
    E: IntoResponse,
{
    /// Call the [`MethodRouter`] without [`ServerContext`].
    ///
    /// This function will generate an empty [`ServerContext`] and use it.
    pub async fn call_without_cx(&self, req: Request<B>) -> Result<Response, E> {
        self.call(&mut ServerContext::new(mock_address()), req)
            .await
    }

    /// Call the [`MethodRouter`] with only [`Method`] and [`Body`].
    pub async fn call_route<D>(&self, method: Method, data: D) -> Response
    where
        B: TryFrom<D>,
        B::Error: Debug,
    {
        self.call_without_cx(
            Request::builder()
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
    S: Service<ServerContext, Request<B>>,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
{
    /// Call the [`TestServer`] as a [`Service`].
    pub async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> Result<S::Response, S::Error> {
        self.inner.call(cx, req).await
    }

    /// Call the [`TestServer`] without [`ServerContext`].
    ///
    /// This function will generate an empty [`ServerContext`] and use it.
    pub async fn call_without_cx(&self, req: Request<B>) -> Result<S::Response, S::Error> {
        self.call(&mut ServerContext::new(mock_address()), req)
            .await
    }

    /// Call the [`TestServer`] with only [`Method`], [`Uri`] and [`Body`].
    ///
    /// [`Uri`]: http::uri::Uri
    pub async fn call_route<U, D>(&self, method: Method, uri: U, data: D) -> Response
    where
        U: AsRef<str>,
        B: TryFrom<D>,
        B::Error: Debug,
    {
        self.call_without_cx(
            Request::builder()
                .method(method)
                .uri(uri.as_ref().to_owned())
                .body(B::try_from(data).expect("Failed to convert data to body"))
                .expect("Failed to build request"),
        )
        .await
        .into_response()
    }
}

impl<S, L, SP> Server<S, L, SP> {
    /// Consume the current server and generate a test server.
    ///
    /// This should be used for unit test only.
    pub fn into_test_server<B>(self) -> TestServer<L::Service, B>
    where
        S: Service<ServerContext, Request<B>>,
        L: Layer<S>,
    {
        TestServer {
            inner: self.layer.layer(self.service),
            _marker: PhantomData,
        }
    }
}
