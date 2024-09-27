use std::sync::Arc;

use faststr::FastStr;
use http::{header, header::HeaderValue, status::StatusCode};
use motore::{
    layer::{Identity, Layer},
    service::{BoxService, Service},
};
use volo::{client::MkClient, context::Endpoint};

use super::{
    callopt::CallOpt, meta::MetaService, Client, ClientBuilder, ClientInner, Target,
    PKG_NAME_WITH_VER,
};
use crate::{
    context::client::{ClientContext, Config},
    error::ClientError,
    request::ClientRequest,
    response::ClientResponse,
    utils::test_helpers::mock_address,
};

/// Default mock service of [`Client`]
pub type ClientMockService = MetaService<MockTransport>;
/// Default [`Client`] without any extra [`Layer`]s
pub type DefaultMockClient<IL = Identity, OL = Identity> =
    Client<<OL as Layer<<IL as Layer<ClientMockService>>::Service>>::Service>;

/// Mock transport [`Service`] without any network connection.
pub enum MockTransport {
    /// Always return a default [`ClientResponse`] with given [`StatusCode`], `HTTP/1.1` and
    /// nothing in headers and body.
    Status(StatusCode),
    /// A [`Service`] for processing the request.
    Service(BoxService<ClientContext, ClientRequest, ClientResponse, ClientError>),
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

    /// Create a [`MockTransport`] that always return a default [`ClientResponse`] with given
    /// [`StatusCode`], `HTTP/1.1` and nothing in headers and body.
    pub fn status_code(status: StatusCode) -> Self {
        Self::Status(status)
    }

    /// Create a [`MockTransport`] from a [`Service`] with [`ClientContext`] and [`ClientRequest`].
    pub fn service<S>(service: S) -> Self
    where
        S: Service<ClientContext, ClientRequest, Response = ClientResponse, Error = ClientError>
            + Send
            + Sync
            + 'static,
    {
        Self::Service(BoxService::new(service))
    }

    /// Create a [`MockTransport`] from a [`Service`] with [`ServerContext`] and [`ServerRequest`].
    ///
    /// Note that all of [`Router`], [`MethodRouter`] and [`Route`] are server [`Service`], they
    /// can be used here.
    ///
    /// [`ServerContext`]: crate::context::ServerContext
    /// [`ServerRequest`]: crate::request::ServerRequest
    /// [`Router`]: crate::server::route::Router
    /// [`MethodRouter`]: crate::server::route::MethodRouter
    /// [`Route`]: crate::server::route::Route
    #[cfg(feature = "server")]
    pub fn server_service<S>(service: S) -> Self
    where
        S: Service<
                crate::context::ServerContext,
                crate::request::ServerRequest,
                Response = crate::response::ServerResponse,
            > + Send
            + Sync
            + 'static,
        S::Error: Into<crate::error::BoxError>,
    {
        Self::Service(BoxService::new(
            crate::utils::test_helpers::ConvertService::new(service),
        ))
    }
}

impl Service<ClientContext, ClientRequest> for MockTransport {
    type Response = ClientResponse;
    type Error = ClientError;

    async fn call(
        &self,
        cx: &mut ClientContext,
        req: ClientRequest,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Status(status) => {
                let mut resp = ClientResponse::default();
                status.clone_into(resp.status_mut());
                Ok(resp)
            }
            Self::Service(srv) => srv.call(cx, req).await,
        }
    }
}

impl<IL, OL, C, LB> ClientBuilder<IL, OL, C, LB> {
    /// Build a mock HTTP client with a [`MockTransport`] service.
    pub fn mock(mut self, transport: MockTransport) -> C::Target
    where
        IL: Layer<MetaService<MockTransport>>,
        IL::Service: Send + Sync + 'static,
        // remove loadbalance here
        OL: Layer<IL::Service>,
        OL::Service: Send + Sync + 'static,
        C: MkClient<Client<OL::Service>>,
    {
        let meta_service = MetaService::new(transport);
        let service = self.outer_layer.layer(self.inner_layer.layer(meta_service));

        let caller_name = if self.caller_name.is_empty() {
            FastStr::from_static_str(PKG_NAME_WITH_VER)
        } else {
            self.caller_name
        };
        if !caller_name.is_empty() && self.headers.get(header::USER_AGENT).is_none() {
            self.headers.insert(
                header::USER_AGENT,
                HeaderValue::from_str(caller_name.as_str()).expect("Invalid caller name"),
            );
        }
        let config = Config {
            timeout: self.builder_config.timeout,
            fail_on_error_status: self.builder_config.fail_on_error_status,
        };

        let client_inner = ClientInner {
            caller_name,
            callee_name: self.callee_name,
            // set a default target so that we can create a request without authority
            default_target: Target::from_address(mock_address()),
            default_config: config,
            default_call_opt: self.call_opt,
            // do nothing
            target_parser: parse_target,
            headers: self.headers,
        };
        let client = Client {
            service,
            inner: Arc::new(client_inner),
        };
        self.mk_client.mk_client(client)
    }
}

// do nothing
fn parse_target(_: Target, _: Option<&CallOpt>, _: &mut Endpoint) {}

#[allow(unused)]
fn client_types_check() {
    struct TestLayer;
    struct TestService<S> {
        inner: S,
    }

    impl<S> Layer<S> for TestLayer {
        type Service = TestService<S>;

        fn layer(self, inner: S) -> Self::Service {
            TestService { inner }
        }
    }

    impl<S, Cx, Req> Service<Cx, Req> for TestService<S>
    where
        S: Service<Cx, Req>,
    {
        type Response = S::Response;
        type Error = S::Error;

        fn call(
            &self,
            cx: &mut Cx,
            req: Req,
        ) -> impl std::future::Future<Output = Result<Self::Response, Self::Error>> + Send {
            self.inner.call(cx, req)
        }
    }

    let _: DefaultMockClient = ClientBuilder::new().mock(Default::default());
    let _: DefaultMockClient<TestLayer> = ClientBuilder::new()
        .layer_inner(TestLayer)
        .mock(Default::default());
    let _: DefaultMockClient<TestLayer> = ClientBuilder::new()
        .layer_inner_front(TestLayer)
        .mock(Default::default());
    let _: DefaultMockClient<Identity, TestLayer> = ClientBuilder::new()
        .layer_outer(TestLayer)
        .mock(Default::default());
    let _: DefaultMockClient<Identity, TestLayer> = ClientBuilder::new()
        .layer_outer_front(TestLayer)
        .mock(Default::default());
    let _: DefaultMockClient<TestLayer, TestLayer> = ClientBuilder::new()
        .layer_inner(TestLayer)
        .layer_outer(TestLayer)
        .mock(Default::default());
}

mod mock_transport_tests {
    use http::status::StatusCode;

    use super::MockTransport;
    use crate::{body::BodyConversion, ClientBuilder};

    #[tokio::test]
    async fn empty_response_test() {
        let client = ClientBuilder::new().mock(MockTransport::default());
        let resp = client.get("/").unwrap().send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().is_empty());
        assert!(resp.into_body().into_vec().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_response_test() {
        {
            let client =
                ClientBuilder::new().mock(MockTransport::status_code(StatusCode::IM_A_TEAPOT));
            let resp = client.get("/").unwrap().send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
            assert!(resp.headers().is_empty());
            assert!(resp.into_body().into_vec().await.unwrap().is_empty());
        }
        {
            let client = {
                let mut builder = ClientBuilder::new();
                builder.fail_on_error_status(true);
                builder.mock(MockTransport::status_code(StatusCode::IM_A_TEAPOT))
            };
            assert!(client.get("/").unwrap().send().await.is_err());
        }
    }
}
