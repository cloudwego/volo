//! Multi-service router for volo-thrift.
//!
//! This module provides a [`Router`] that allows a single server to handle
//! multiple Thrift services, routing requests based on the IDL service name
//! (`isn`) field in TTHeader.
//!
//! # Overview
//!
//! The multi-service feature enables serving multiple Thrift services from a single
//! server instance. Requests are routed based on the `isn` (IDL Service Name) field
//! in the TTHeader protocol.
//!
//! # Routing Rules
//!
//! 1. If the request has an `isn` that matches a registered service, route to that service
//! 2. If the request has no `isn` or an unknown `isn`, route to the default service
//! 3. If there's no default service and no match, return an error
//!
//! # Example
//!
//! ```ignore
//! use volo_thrift::server::{Router, Server};
//!
//! // Create service implementations
//! let hello_service = HelloServiceServer { inner: HelloImpl };
//! let echo_service = EchoServiceServer { inner: EchoImpl };
//!
//! // Build the router
//! let router = Router::new()
//!     .with_default_service(hello_service)  // Default for requests without ISN
//!     .add_service(echo_service);           // Routes when ISN = "EchoService"
//!
//! // Run the server
//! Server::with_router(router)
//!     .run(addr)
//!     .await?;
//! ```
//!
//! # Client-side ISN
//!
//! Clients can specify the target service by setting the `isn` field in metainfo:
//!
//! ```ignore
//! use volo_thrift::codec::default::ttheader::HEADER_IDL_SERVICE_NAME;
//!
//! metainfo::METAINFO.with(|mi| {
//!     mi.borrow_mut().set_persistent(
//!         HEADER_IDL_SERVICE_NAME.into(),
//!         "EchoService".into(),
//!     );
//! });
//! ```

use ahash::AHashMap;
use motore::{BoxCloneService, service::Service};
use pilota::thrift::{ApplicationException, ApplicationExceptionKind};
use volo::FastStr;

use crate::{
    Bytes, ServerError,
    context::{ServerContext, ThriftContext},
};

/// A trait to provide a static reference to the service's name.
///
/// This is used for routing services within the router.
/// The name should match the service name defined in the Thrift IDL.
///
/// # Example
///
/// ```ignore
/// impl NamedService for MyServiceServer<S> {
///     const NAME: &'static str = "MyService";
/// }
/// ```
pub trait NamedService {
    /// The service name as defined in the Thrift IDL.
    const NAME: &'static str;
}

type BoxedService = BoxCloneService<ServerContext, Bytes, Bytes, ServerError>;

/// A router for multiple Thrift services.
///
/// The router dispatches requests to the appropriate service based on the
/// IDL service name (`isn`) field in TTHeader. If no `isn` is present or
/// the service name is not found, the request is routed to the default service.
///
/// # Example
///
/// ```ignore
/// use volo_thrift::server::{Router, Server};
///
/// let service_a = volo_gen::a::ServiceAServer::new(ImplA);
/// let service_b = volo_gen::b::ServiceBServer::new(ImplB);
///
/// let router = Router::new()
///     .with_default_service(service_a)  // Default service (handles requests without ISN)
///     .add_service(service_b);          // Additional service
///
/// Server::with_router(router)
///     .run(addr)
///     .await?;
/// ```
pub struct Router {
    services: AHashMap<FastStr, BoxedService>,
    default_service: Option<BoxedService>,
}

impl Default for Router {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Router {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            services: self.services.clone(),
            default_service: self.default_service.clone(),
        }
    }
}

impl Router {
    /// Creates a new empty router.
    pub fn new() -> Self {
        Self {
            services: AHashMap::new(),
            default_service: None,
        }
    }

    /// Sets the default service for the router.
    ///
    /// The default service handles requests that either:
    /// - Don't have an `isn` field in TTHeader
    /// - Have an `isn` that doesn't match any registered service
    ///
    /// The service is also registered by its name for explicit routing.
    pub fn with_default_service<S>(mut self, service: S) -> Self
    where
        S: Service<ServerContext, Bytes, Response = Bytes, Error = ServerError>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let name = FastStr::from_static_str(S::NAME);
        let boxed = BoxCloneService::new(service);
        self.default_service = Some(boxed.clone());
        self.services.insert(name, boxed);
        self
    }

    /// Adds a service to the router.
    ///
    /// The service will be routed to when the `isn` field in TTHeader
    /// matches the service's name (from [`NamedService::NAME`]).
    pub fn add_service<S>(mut self, service: S) -> Self
    where
        S: Service<ServerContext, Bytes, Response = Bytes, Error = ServerError>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let name = FastStr::from_static_str(S::NAME);
        self.services.insert(name, BoxCloneService::new(service));
        self
    }

    /// Returns the number of registered services.
    pub fn service_count(&self) -> usize {
        self.services.len()
    }

    /// Returns whether the router has a default service.
    pub fn has_default_service(&self) -> bool {
        self.default_service.is_some()
    }
}

impl Service<ServerContext, Bytes> for Router {
    type Response = Bytes;
    type Error = ServerError;

    #[inline]
    async fn call(
        &self,
        cx: &mut ServerContext,
        payload: Bytes,
    ) -> Result<Self::Response, Self::Error> {
        // Get the IDL service name from context (set by TTHeader decoder)
        let service_name = cx.idl_service_name();

        let service = match service_name {
            Some(name) => self.services.get(name).or(self.default_service.as_ref()),
            None => self.default_service.as_ref(),
        };

        match service {
            Some(svc) => svc.call(cx, payload).await,
            None => Err(ServerError::Application(ApplicationException::new(
                ApplicationExceptionKind::UNKNOWN_METHOD,
                format!(
                    "service not found: {:?}",
                    service_name.map(|s: &FastStr| s.as_str())
                ),
            ))),
        }
    }
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .field("has_default_service", &self.default_service.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock service that returns a fixed response with its name
    #[derive(Clone)]
    struct MockService {
        name: &'static str,
    }

    impl NamedService for MockService {
        const NAME: &'static str = "MockService";
    }

    impl Service<ServerContext, Bytes> for MockService {
        type Response = Bytes;
        type Error = ServerError;

        async fn call(
            &self,
            _cx: &mut ServerContext,
            _payload: Bytes,
        ) -> Result<Self::Response, Self::Error> {
            Ok(Bytes::from(self.name))
        }
    }

    /// Another mock service with a different name
    #[derive(Clone)]
    struct AnotherMockService;

    impl NamedService for AnotherMockService {
        const NAME: &'static str = "AnotherService";
    }

    impl Service<ServerContext, Bytes> for AnotherMockService {
        type Response = Bytes;
        type Error = ServerError;

        async fn call(
            &self,
            _cx: &mut ServerContext,
            _payload: Bytes,
        ) -> Result<Self::Response, Self::Error> {
            Ok(Bytes::from("another"))
        }
    }

    #[test]
    fn test_router_new() {
        let router = Router::new();
        assert_eq!(router.service_count(), 0);
        assert!(!router.has_default_service());
    }

    #[test]
    fn test_router_with_default_service() {
        let router = Router::new().with_default_service(MockService { name: "default" });
        assert_eq!(router.service_count(), 1);
        assert!(router.has_default_service());
    }

    #[test]
    fn test_router_add_service() {
        let router = Router::new()
            .with_default_service(MockService { name: "default" })
            .add_service(AnotherMockService);
        assert_eq!(router.service_count(), 2);
        assert!(router.has_default_service());
    }

    #[tokio::test]
    async fn test_router_routes_by_isn() {
        let router = Router::new()
            .with_default_service(MockService { name: "default" })
            .add_service(AnotherMockService);

        let mut cx = ServerContext::default();
        // Set ISN to "AnotherService"
        cx.set_idl_service_name(FastStr::from_static_str("AnotherService"));

        let result = router.call(&mut cx, Bytes::new()).await.unwrap();
        assert_eq!(result, Bytes::from("another"));
    }

    #[tokio::test]
    async fn test_router_routes_to_default_without_isn() {
        let router = Router::new()
            .with_default_service(MockService { name: "default" })
            .add_service(AnotherMockService);

        let mut cx = ServerContext::default();
        // No ISN set

        let result = router.call(&mut cx, Bytes::new()).await.unwrap();
        assert_eq!(result, Bytes::from("default"));
    }

    #[tokio::test]
    async fn test_router_routes_to_default_with_unknown_isn() {
        let router = Router::new()
            .with_default_service(MockService { name: "default" })
            .add_service(AnotherMockService);

        let mut cx = ServerContext::default();
        // Set ISN to unknown service
        cx.set_idl_service_name(FastStr::from_static_str("UnknownService"));

        let result = router.call(&mut cx, Bytes::new()).await.unwrap();
        assert_eq!(result, Bytes::from("default"));
    }

    #[tokio::test]
    async fn test_router_error_no_service_found() {
        let router = Router::new().add_service(AnotherMockService);
        // No default service

        let mut cx = ServerContext::default();
        // Set ISN to unknown service
        cx.set_idl_service_name(FastStr::from_static_str("UnknownService"));

        let result = router.call(&mut cx, Bytes::new()).await;
        assert!(result.is_err());
        match result {
            Err(ServerError::Application(e)) => {
                assert_eq!(e.kind(), ApplicationExceptionKind::UNKNOWN_METHOD);
                assert!(e.message().contains("UnknownService"));
            }
            _ => panic!("Expected ApplicationException"),
        }
    }

    #[tokio::test]
    async fn test_router_error_no_default_no_isn() {
        let router = Router::new().add_service(AnotherMockService);
        // No default service

        let mut cx = ServerContext::default();
        // No ISN set

        let result = router.call(&mut cx, Bytes::new()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_router_routes_to_named_service_by_isn() {
        let router = Router::new()
            .with_default_service(MockService { name: "default" })
            .add_service(AnotherMockService);

        let mut cx = ServerContext::default();
        // Set ISN to "MockService" (the default service's name)
        cx.set_idl_service_name(FastStr::from_static_str("MockService"));

        let result = router.call(&mut cx, Bytes::new()).await.unwrap();
        assert_eq!(result, Bytes::from("default"));
    }

    #[test]
    fn test_router_clone() {
        let router = Router::new()
            .with_default_service(MockService { name: "default" })
            .add_service(AnotherMockService);

        let cloned = router.clone();
        assert_eq!(cloned.service_count(), 2);
        assert!(cloned.has_default_service());
    }

    #[test]
    fn test_router_debug() {
        let router = Router::new()
            .with_default_service(MockService { name: "default" })
            .add_service(AnotherMockService);

        let debug_str = format!("{:?}", router);
        assert!(debug_str.contains("Router"));
        assert!(debug_str.contains("has_default_service: true"));
    }
}
