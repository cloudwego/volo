//! Test utilities for client of Volo-HTTP.

use std::{sync::Arc, time::Duration};

use http::status::StatusCode;
use motore::{
    layer::Layer,
    service::{BoxService, Service},
};
use volo::client::MkClient;

use super::{Client, ClientBuilder, ClientInner};
use crate::{
    body::{Body, BodyConversion},
    context::client::ClientContext,
    error::client::{ClientError, Result, other_error},
    request::{Request, RequestPartsExt},
    response::Response,
};

/// Default mock service of [`Client`]
pub type ClientMockService = MockTransport;

/// Mock transport [`Service`] without any network connection.
pub enum MockTransport {
    /// Always return a default [`Response`] with given [`StatusCode`], `HTTP/1.1` and
    /// nothing in headers and body.
    Status(StatusCode),
    /// A [`Service`] for processing the request.
    Service(BoxService<ClientContext, Request, Response, ClientError>),
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::Status(StatusCode::OK)
    }
}

impl MockTransport {
    /// Create a default [`MockTransport`] that always responds with an empty response.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a [`MockTransport`] that always return a default [`Response`] with given
    /// [`StatusCode`], `HTTP/1.1` and nothing in headers and body.
    pub fn status_code(status: StatusCode) -> Self {
        Self::Status(status)
    }

    /// Create a [`MockTransport`] from a [`Service`] with [`ClientContext`] and [`Request`].
    pub fn service<S>(service: S) -> Self
    where
        S: Service<ClientContext, Request, Response = Response, Error = ClientError>
            + Send
            + Sync
            + 'static,
    {
        Self::Service(BoxService::new(service))
    }

    /// Create a [`MockTransport`] from a [`Service`] with [`ServerContext`] and [`Request`].
    ///
    /// Note that all of [`Router`], [`MethodRouter`] and [`Route`] are server [`Service`], they
    /// can be used here.
    ///
    /// [`ServerContext`]: crate::context::ServerContext
    /// [`Router`]: crate::server::route::Router
    /// [`MethodRouter`]: crate::server::route::MethodRouter
    /// [`Route`]: crate::server::route::Route
    #[cfg(feature = "server")]
    pub fn server_service<S>(service: S) -> Self
    where
        S: Service<crate::context::ServerContext, Request, Response = Response>
            + Send
            + Sync
            + 'static,
        S::Error: Into<crate::error::BoxError>,
    {
        Self::Service(BoxService::new(
            crate::utils::test_helpers::ConvertService::new(service),
        ))
    }
}

impl Service<ClientContext, Request> for MockTransport {
    type Response = Response;
    type Error = ClientError;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: Request,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Status(status) => {
                let mut resp = Response::default();
                status.clone_into(resp.status_mut());
                Ok(resp)
            }
            Self::Service(srv) => srv.call(cx, req).await,
        }
    }
}

impl<IL, OL, C, LB> ClientBuilder<IL, OL, C, LB> {
    /// Build a mock HTTP client with a [`MockTransport`] service.
    pub fn mock<ReqBody, RespBody>(self, transport: MockTransport) -> Result<C::Target>
    where
        IL: Layer<ClientMockService>,
        IL::Service: Send + Sync + 'static,
        // remove loadbalance here
        OL: Layer<IL::Service>,
        OL::Service: Service<
                ClientContext,
                Request<ReqBody>,
                Response = Response<RespBody>,
                Error = ClientError,
            > + Send
            + Sync
            + 'static,
        C: MkClient<Client<ReqBody, RespBody>>,
        ReqBody: Send + 'static,
        RespBody: Send,
    {
        self.status?;

        let meta_service = transport;
        let service = self.outer_layer.layer(self.inner_layer.layer(meta_service));
        let service = BoxService::new(service);

        let client_inner = ClientInner {
            service,
            timeout: self.timeout,
            headers: self.headers,
        };
        let client = Client {
            inner: Arc::new(client_inner),
        };
        Ok(self.mk_client.mk_client(client))
    }
}

/// A [`Layer`] for dumping request and response.
///
/// Note that it will collect request and response as bytes and then dump it, using stream is not
/// suggested.
#[derive(Debug, Default)]
pub enum DebugLayer {
    /// Dump request and response as [`String`].
    #[default]
    DumpString,
    /// Dump request and response as `[u8]`.
    DumpBytes,
}

fn dump_request_parts(parts: &http::request::Parts) {
    if let Some(url) = parts.url() {
        println!("  == {url} ==");
    }

    println!("{:?} {:?} {:?}", parts.method, parts.uri, parts.version);
    for (k, v) in parts.headers.iter() {
        let Ok(v) = v.to_str() else {
            continue;
        };
        println!("{k}: {v}");
    }
}

fn dump_response_parts(parts: &http::response::Parts) {
    println!("{:?} {}", parts.version, parts.status);
    for (k, v) in parts.headers.iter() {
        println!("{k}: {v:?}");
    }
}

impl DebugLayer {
    async fn dump_request(&self, req: Request) -> Result<Request> {
        let (parts, body) = req.into_parts();
        let bytes = body.into_bytes().await?;
        println!(" ==== DebugLayer::dump_request ====");
        dump_request_parts(&parts);
        println!();
        match self {
            DebugLayer::DumpString => {
                let s = std::str::from_utf8(bytes.as_ref()).map_err(other_error)?;
                println!("{s}");
            }
            DebugLayer::DumpBytes => {
                println!("{:?}", bytes.as_ref());
            }
        }
        println!(" ==== DebugLayer::dump_request ====");
        let body = Body::from(bytes);
        Ok(Request::from_parts(parts, body))
    }

    async fn dump_response(&self, resp: Response) -> Result<Response> {
        let (parts, body) = resp.into_parts();
        let bytes = body.into_bytes().await?;
        println!(" ==== DebugLayer::dump_response ====");
        dump_response_parts(&parts);
        println!();
        match self {
            DebugLayer::DumpString => {
                let s = std::str::from_utf8(bytes.as_ref()).map_err(other_error)?;
                println!("{s}");
            }
            DebugLayer::DumpBytes => {
                println!("{:?}", bytes.as_ref());
            }
        }
        println!(" ==== DebugLayer::dump_response ====");
        let body = Body::from(bytes);
        Ok(Response::from_parts(parts, body))
    }
}

impl<S> Layer<S> for DebugLayer {
    type Service = DebugService<S>;

    fn layer(self, inner: S) -> Self::Service {
        DebugService {
            inner,
            config: self,
        }
    }
}

/// [`Service`] generated by [`DebugLayer`].
///
/// For more details, see [`DebugLayer`].
pub struct DebugService<S> {
    inner: S,
    config: DebugLayer,
}

impl<S> Service<ClientContext, Request> for DebugService<S>
where
    S: Service<ClientContext, Request, Response = Response, Error = ClientError> + Send + Sync,
{
    type Response = Response;
    type Error = ClientError;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: Request,
    ) -> Result<Self::Response, Self::Error> {
        let req = self.config.dump_request(req).await?;
        let resp = self.inner.call(cx, req).await?;
        self.config.dump_response(resp).await
    }
}

/// [`Layer`] that will retry when the previous call to inner [`Service`] returned an error status
/// code.
///
/// Note that this layer will collect request and clone it for retry, so it should not be used with
/// a [`Service`] that uses stream.
pub struct RetryOnStatus {
    client_error: bool,
    server_error: bool,
    max_retry: usize,
    sleep_time: Duration,
}

impl RetryOnStatus {
    const DEFAULT_MAX_RETRY: usize = 5;
    const DEFAULT_SLEEP_TIME: Duration = Duration::from_secs(1);

    /// Create a [`RetryOnStatus`] layer that retry for both client error and server error status
    /// code (4XX and 5XX).
    pub fn all() -> Self {
        Self {
            client_error: true,
            server_error: true,
            max_retry: Self::DEFAULT_MAX_RETRY,
            sleep_time: Self::DEFAULT_SLEEP_TIME,
        }
    }

    /// Create a [`RetryOnStatus`] layer that retry for client error status code (4XX).
    pub fn client_error() -> Self {
        Self {
            client_error: true,
            server_error: true,
            max_retry: Self::DEFAULT_MAX_RETRY,
            sleep_time: Self::DEFAULT_SLEEP_TIME,
        }
    }

    /// Create a [`RetryOnStatus`] layer that retry for server error status code (5XX).
    pub fn server_error() -> Self {
        Self {
            client_error: true,
            server_error: true,
            max_retry: Self::DEFAULT_MAX_RETRY,
            sleep_time: Self::DEFAULT_SLEEP_TIME,
        }
    }

    /// Set num of max retry times for current layer.
    ///
    /// Default is 5.
    pub fn with_max_retry(mut self, max_retry: usize) -> Self {
        self.max_retry = max_retry;
        self
    }

    /// Set sleep time between retries.
    ///
    /// Default is 1s.
    pub fn with_sleep_time(mut self, sleep_time: Duration) -> Self {
        self.sleep_time = sleep_time;
        self
    }
}

impl<S> Layer<S> for RetryOnStatus {
    type Service = RetryOnStatusService<S>;

    fn layer(self, inner: S) -> Self::Service {
        RetryOnStatusService {
            inner,
            config: self,
        }
    }
}

/// [`RetryOnStatus`] generated [`Service`], refer to [`RetryOnStatus`] for more details.
pub struct RetryOnStatusService<S> {
    inner: S,
    config: RetryOnStatus,
}

impl<S> Service<ClientContext, Request> for RetryOnStatusService<S>
where
    S: Service<ClientContext, Request, Response = Response, Error = ClientError> + Send + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: Request,
    ) -> Result<Self::Response, Self::Error> {
        let (parts, body) = req.into_parts();
        let bytes_body = body.into_bytes().await?;
        let mut retry = 0;
        loop {
            let req = Request::from_parts(parts.clone(), Body::from(bytes_body.clone()));
            let resp = self.inner.call(cx, req).await?;
            if (retry < self.config.max_retry)
                && ((resp.status().is_client_error() && self.config.client_error)
                    || (resp.status().is_server_error() && self.config.server_error))
            {
                retry += 1;
                tokio::time::sleep(self.config.sleep_time).await;
                println!("retry on \"{}\" for {retry} time(s)", parts.uri);
            } else {
                return Ok(resp);
            }
        }
    }
}

mod mock_transport_tests {
    use http::status::StatusCode;

    use super::MockTransport;
    use crate::{ClientBuilder, body::BodyConversion};

    #[tokio::test]
    async fn empty_response_test() {
        let client = ClientBuilder::new().mock(MockTransport::default()).unwrap();
        let resp = client.get("/").send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().is_empty());
        assert!(resp.into_body().into_vec().await.unwrap().is_empty());
    }
}
