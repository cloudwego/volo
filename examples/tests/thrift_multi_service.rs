//! Integration tests for volo-thrift multi-service routing.
//!
//! This test demonstrates how to use the Router to serve multiple Thrift services
//! from a single server, routing requests based on the IDL service name (ISN) header.

use std::{net::SocketAddr, time::Duration};

use tokio::sync::oneshot;
use volo_thrift::server::{Router, Server};

/// HelloService implementation
#[derive(Clone)]
struct HelloServiceImpl;

impl volo_gen::thrift_gen::hello::HelloService for HelloServiceImpl {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        Ok(volo_gen::thrift_gen::hello::HelloResponse {
            message: format!("Hello, {}!", req.name).into(),
            _field_mask: None,
        })
    }
}

/// EchoService implementation
#[derive(Clone)]
struct EchoServiceImpl;

impl volo_gen::thrift_gen::echo::EchoService for EchoServiceImpl {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::echo::EchoRequest,
    ) -> Result<volo_gen::thrift_gen::echo::EchoResponse, volo_thrift::ServerError> {
        Ok(volo_gen::thrift_gen::echo::EchoResponse {
            faststr_with_default: req.faststr_with_default,
            faststr: req.faststr,
            name: format!("Echo: {}", req.name).into(),
            map_with_default: req.map_with_default,
            map: req.map,
            echo_union: req.echo_union,
            echo_enum: req.echo_enum,
            _field_mask: None,
        })
    }
}

/// Find an available port for testing
async fn find_available_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    // Small delay to ensure port is released
    tokio::time::sleep(Duration::from_millis(10)).await;
    port
}

/// Start a multi-service server and return a shutdown signal sender
async fn start_multi_service_server(port: u16) -> oneshot::Sender<()> {
    let (tx, rx) = oneshot::channel::<()>();

    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let addr = volo::net::Address::from(addr);

    // Create the services using from_handler - the generated *Server types implement
    // Service<ServerContext, Bytes> for multi-service routing
    let hello_service =
        volo_gen::thrift_gen::hello::HelloServiceServer::from_handler(HelloServiceImpl);
    let echo_service = volo_gen::thrift_gen::echo::EchoServiceServer::from_handler(EchoServiceImpl);

    // Create a router with HelloService as default and EchoService as additional service
    let router = Router::new()
        .with_default_service(hello_service)
        .add_service(echo_service);

    // Spawn the server
    tokio::spawn(async move {
        let server = Server::with_router(router);
        tokio::select! {
            result = server.run(addr) => {
                if let Err(e) = result {
                    eprintln!("Server error: {:?}", e);
                }
            }
            _ = rx => {
                // Shutdown signal received
            }
        }
    });

    // Wait a bit for the server to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    tx
}

/// Test that the server can be started with a router and respond to requests
/// This test routes to the default HelloService (no ISN set by client)
#[tokio::test]
async fn test_multi_service_router_default_service() {
    let port = find_available_port().await;
    let shutdown = start_multi_service_server(port).await;

    // Create a client for HelloService
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let client = volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(addr)
        .build();

    // Call the service - should route to default (HelloService) since no ISN is set
    let req = volo_gen::thrift_gen::hello::HelloRequest {
        name: "World".into(),
        hello: None,
        _field_mask: None,
    };

    let resp = client.hello(req).await;
    assert!(
        resp.is_ok(),
        "Expected successful response, got: {:?}",
        resp
    );
    let resp = resp.unwrap();
    assert_eq!(resp.message.as_str(), "Hello, World!");

    // Shutdown the server
    let _ = shutdown.send(());
}

/// Test router service registration and counting
#[tokio::test]
async fn test_router_service_count() {
    // Create the services using from_handler
    let hello_service =
        volo_gen::thrift_gen::hello::HelloServiceServer::from_handler(HelloServiceImpl);
    let echo_service = volo_gen::thrift_gen::echo::EchoServiceServer::from_handler(EchoServiceImpl);

    // Test router creation
    let router = Router::new();
    assert_eq!(router.service_count(), 0);
    assert!(!router.has_default_service());

    // Add default service
    let router = router.with_default_service(hello_service);
    assert_eq!(router.service_count(), 1);
    assert!(router.has_default_service());

    // Add another service
    let router = router.add_service(echo_service);
    assert_eq!(router.service_count(), 2);
    assert!(router.has_default_service());
}

/// Test that the generated servers implement NamedService trait correctly
#[tokio::test]
async fn test_named_service_trait() {
    use volo_thrift::server::NamedService;

    // Check HelloServiceServer
    assert_eq!(
        <volo_gen::thrift_gen::hello::HelloServiceServer<HelloServiceImpl> as NamedService>::NAME,
        "HelloService"
    );

    // Check EchoServiceServer
    assert_eq!(
        <volo_gen::thrift_gen::echo::EchoServiceServer<EchoServiceImpl> as NamedService>::NAME,
        "EchoService"
    );
}

/// Test router routing logic directly using Service trait
#[tokio::test]
async fn test_router_direct_routing() {
    use motore::service::Service;
    use volo_thrift::context::ThriftContext;

    // Create mock services that return identifiable responses
    #[derive(Clone)]
    struct MockHelloService;
    impl volo_thrift::server::NamedService for MockHelloService {
        const NAME: &'static str = "HelloService";
    }
    impl Service<volo_thrift::context::ServerContext, volo_thrift::Bytes> for MockHelloService {
        type Response = volo_thrift::Bytes;
        type Error = volo_thrift::ServerError;
        async fn call(
            &self,
            _cx: &mut volo_thrift::context::ServerContext,
            _payload: volo_thrift::Bytes,
        ) -> Result<Self::Response, Self::Error> {
            Ok(volo_thrift::Bytes::from_static(b"hello_response"))
        }
    }

    #[derive(Clone)]
    struct MockEchoService;
    impl volo_thrift::server::NamedService for MockEchoService {
        const NAME: &'static str = "EchoService";
    }
    impl Service<volo_thrift::context::ServerContext, volo_thrift::Bytes> for MockEchoService {
        type Response = volo_thrift::Bytes;
        type Error = volo_thrift::ServerError;
        async fn call(
            &self,
            _cx: &mut volo_thrift::context::ServerContext,
            _payload: volo_thrift::Bytes,
        ) -> Result<Self::Response, Self::Error> {
            Ok(volo_thrift::Bytes::from_static(b"echo_response"))
        }
    }

    let router = Router::new()
        .with_default_service(MockHelloService)
        .add_service(MockEchoService);

    // Test 1: No ISN - should route to default (HelloService)
    {
        let mut cx = volo_thrift::context::ServerContext::default();
        let result = router.call(&mut cx, volo_thrift::Bytes::new()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"hello_response");
    }

    // Test 2: ISN = "EchoService" - should route to EchoService
    {
        let mut cx = volo_thrift::context::ServerContext::default();
        cx.set_idl_service_name(volo::FastStr::from_static_str("EchoService"));
        let result = router.call(&mut cx, volo_thrift::Bytes::new()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"echo_response");
    }

    // Test 3: ISN = "HelloService" - should route to HelloService
    {
        let mut cx = volo_thrift::context::ServerContext::default();
        cx.set_idl_service_name(volo::FastStr::from_static_str("HelloService"));
        let result = router.call(&mut cx, volo_thrift::Bytes::new()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"hello_response");
    }

    // Test 4: Unknown ISN - should fall back to default
    {
        let mut cx = volo_thrift::context::ServerContext::default();
        cx.set_idl_service_name(volo::FastStr::from_static_str("UnknownService"));
        let result = router.call(&mut cx, volo_thrift::Bytes::new()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"hello_response");
    }
}

/// Start a multi-service server with multiplex enabled
#[cfg(feature = "multiplex")]
async fn start_multi_service_server_multiplex(port: u16) -> oneshot::Sender<()> {
    let (tx, rx) = oneshot::channel::<()>();

    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let addr = volo::net::Address::from(addr);

    // Create the services using from_handler
    let hello_service =
        volo_gen::thrift_gen::hello::HelloServiceServer::from_handler(HelloServiceImpl);
    let echo_service = volo_gen::thrift_gen::echo::EchoServiceServer::from_handler(EchoServiceImpl);

    // Create a router
    let router = Router::new()
        .with_default_service(hello_service)
        .add_service(echo_service);

    // Spawn the server with multiplex enabled
    tokio::spawn(async move {
        let server = Server::with_router(router).multiplex(true);
        tokio::select! {
            result = server.run(addr) => {
                if let Err(e) = result {
                    eprintln!("Server error: {:?}", e);
                }
            }
            _ = rx => {
                // Shutdown signal received
            }
        }
    });

    // Wait a bit for the server to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    tx
}

/// Test multi-service router with multiplex transport
/// This test verifies that Router works correctly when multiplex is enabled
#[cfg(feature = "multiplex")]
#[tokio::test]
async fn test_multi_service_router_with_multiplex() {
    let port = find_available_port().await;
    let shutdown = start_multi_service_server_multiplex(port).await;

    // Create a client for HelloService (with multiplex enabled)
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let client = volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(addr)
        .multiplex(true)
        .build();

    // Call the service - should route to default (HelloService) since no ISN is set
    let req = volo_gen::thrift_gen::hello::HelloRequest {
        name: "MultiplexWorld".into(),
        hello: None,
        _field_mask: None,
    };

    let resp = client.hello(req).await;
    assert!(
        resp.is_ok(),
        "Expected successful response with multiplex, got: {:?}",
        resp
    );
    let resp = resp.unwrap();
    assert_eq!(resp.message.as_str(), "Hello, MultiplexWorld!");

    // Shutdown the server
    let _ = shutdown.send(());
}

/// Test router error when no default service and unknown ISN
#[tokio::test]
async fn test_router_no_default_error() {
    use motore::service::Service;
    use volo_thrift::context::ThriftContext;

    #[derive(Clone)]
    struct MockService;
    impl volo_thrift::server::NamedService for MockService {
        const NAME: &'static str = "MockService";
    }
    impl Service<volo_thrift::context::ServerContext, volo_thrift::Bytes> for MockService {
        type Response = volo_thrift::Bytes;
        type Error = volo_thrift::ServerError;
        async fn call(
            &self,
            _cx: &mut volo_thrift::context::ServerContext,
            _payload: volo_thrift::Bytes,
        ) -> Result<Self::Response, Self::Error> {
            Ok(volo_thrift::Bytes::new())
        }
    }

    // Router without default service
    let router = Router::new().add_service(MockService);

    // Test: Unknown ISN with no default - should error
    let mut cx = volo_thrift::context::ServerContext::default();
    cx.set_idl_service_name(volo::FastStr::from_static_str("UnknownService"));
    let result = router.call(&mut cx, volo_thrift::Bytes::new()).await;
    assert!(result.is_err());

    // Test: No ISN with no default - should error
    let mut cx = volo_thrift::context::ServerContext::default();
    let result = router.call(&mut cx, volo_thrift::Bytes::new()).await;
    assert!(result.is_err());

    // Test: Correct ISN - should succeed
    let mut cx = volo_thrift::context::ServerContext::default();
    cx.set_idl_service_name(volo::FastStr::from_static_str("MockService"));
    let result = router.call(&mut cx, volo_thrift::Bytes::new()).await;
    assert!(result.is_ok());
}
