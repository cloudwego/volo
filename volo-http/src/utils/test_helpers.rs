//! Generic test utilities.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use http::{method::Method, request::Request};
use volo::net::Address;

#[cfg(all(feature = "client", feature = "server"))]
pub use self::convert_service::{ConvertService, client_cx_to_server_cx};

/// Create a simple address, the address is `127.0.0.1:8000`.
pub fn mock_address() -> Address {
    Address::Ip(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        8000,
    ))
}

/// Create a simple [`Request`] with only [`Method`], [`Uri`] and [`Body`].
///
/// [`Uri`]: http::uri::Uri
pub fn simple_req<S, B>(method: Method, uri: S, body: B) -> Request<B>
where
    S: AsRef<str>,
{
    Request::builder()
        .method(method)
        .uri(uri.as_ref())
        .body(body)
        .expect("Failed to build request")
}

#[cfg(all(feature = "client", feature = "server"))]
mod convert_service {
    use motore::service::Service;
    use volo::context::{Context, Endpoint, Role, RpcCx, RpcInfo};

    use super::mock_address;
    use crate::{
        context::{ClientContext, ServerContext, server::ServerCxInner},
        error::{BoxError, ClientError, client::request_error},
        request::Request,
        response::Response,
    };

    /// A wrapper that can convert a [`Service`] with [`ServerContext`] and [`Request`] to
    /// [`ClientContext`] and [`Request`].
    pub struct ConvertService<S> {
        inner: S,
    }

    impl<S> ConvertService<S> {
        /// Create a [`ConvertService`] by a [`Service`] with [`ServerContext`] and
        /// [`Request`].
        pub fn new(inner: S) -> Self {
            Self { inner }
        }
    }

    impl<S> Service<ClientContext, Request> for ConvertService<S>
    where
        S: Service<ServerContext, Request, Response = Response> + Send + Sync,
        S::Error: Into<BoxError>,
    {
        type Response = Response;
        type Error = ClientError;

        async fn call(
            &self,
            cx: &mut ClientContext,
            req: Request,
        ) -> Result<Self::Response, Self::Error> {
            let mut server_cx = client_cx_to_server_cx(cx);
            self.inner
                .call(&mut server_cx, req)
                .await
                .map_err(request_error)
        }
    }

    fn endpoint_clone(ep: &Endpoint) -> Endpoint {
        Endpoint {
            service_name: ep.service_name.clone(),
            address: ep.address.clone(),
            faststr_tags: Default::default(),
            tags: Default::default(),
        }
    }

    #[cfg(not(feature = "__tls"))]
    fn new_server_config(_: &ClientContext) -> crate::context::server::Config {
        crate::context::server::Config::default()
    }

    #[cfg(feature = "__tls")]
    fn new_server_config(client_cx: &ClientContext) -> crate::context::server::Config {
        let mut config = crate::context::server::Config::default();
        if client_cx.rpc_info().callee().get::<http::uri::Scheme>()
            == Some(&http::uri::Scheme::HTTPS)
        {
            config.set_tls(true);
        }
        config
    }

    /// Convert a [`ClientContext`] to [`ServerContext`] with copy [`Endpoint`]s and TLS status.
    pub fn client_cx_to_server_cx(client_cx: &ClientContext) -> ServerContext {
        let client_rpc_info = client_cx.rpc_info();
        let mut server_rpc_info = RpcInfo::new(
            Role::Server,
            client_rpc_info.method().clone(),
            endpoint_clone(client_rpc_info.caller()),
            endpoint_clone(client_rpc_info.callee()),
            new_server_config(client_cx),
        );
        if server_rpc_info.caller().address().is_none() {
            server_rpc_info.caller_mut().set_address(mock_address());
        }
        let server_rpc_cx = RpcCx::new(server_rpc_info, ServerCxInner::default());
        ServerContext(server_rpc_cx)
    }
}

#[cfg(all(feature = "client", feature = "server"))]
mod helper_tests {
    use http::status::StatusCode;

    use crate::{
        body::BodyConversion,
        client::{ClientBuilder, test_helpers::MockTransport},
        server::route::{Router, get},
    };

    const HELLO_WORLD: &str = "Hello, World";

    #[tokio::test]
    async fn client_call_router() {
        let router: Router = Router::new().route("/get", get(|| async { HELLO_WORLD }));
        let client = ClientBuilder::new()
            .mock(MockTransport::server_service(router))
            .unwrap();
        {
            let ret = client
                .get("/get")
                .send()
                .await
                .unwrap()
                .into_string()
                .await
                .unwrap();
            assert_eq!(ret, HELLO_WORLD);
        }
        {
            let ret = client
                .get("http://127.0.0.1/get")
                .send()
                .await
                .unwrap()
                .into_string()
                .await
                .unwrap();
            assert_eq!(ret, HELLO_WORLD);
        }
        {
            let resp = client.get("/").send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        }
        {
            let resp = client.post("/get").send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        }
    }
}
