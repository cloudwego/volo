use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use http_body::Body as HttpBody;
use motore::{BoxCloneService, Service};
use rustc_hash::FxHashMap;
use volo::{FastStr, Unwrap};

use super::{NamedService, policy::ResponsePolicy};
use crate::{
    Request, Response, Status,
    body::BoxBody,
    context::{Deadline, ServerContext},
    message::{MethodDescriptor, PayloadKind, ServiceDescriptor},
    streaming::{CallGuard, HookFactory, StreamContext},
};

type RoutedService<B> = (
    BoxCloneService<ServerContext, Request<B>, Response<BoxBody>, Status>,
    Option<ServiceDescriptor>,
    &'static str,
    Option<HookFactory>,
    bool,
);

/// The requested method has been accepted by a registered service or entry decoder.
/// Older generated services without descriptors publish this after entry dispatch.
#[derive(Clone, Copy, Debug)]
pub struct RegisteredRoute;

/// A native protocol binding selected by a shared, read-only directory.
#[doc(hidden)]
pub struct ServiceRoute<B = BoxBody> {
    inner: RoutedService<B>,
    canonical_path: Option<FastStr>,
    resolved_method: Option<&'static MethodDescriptor>,
}

impl<B> Clone for ServiceRoute<B> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            canonical_path: self.canonical_path.clone(),
            resolved_method: self.resolved_method,
        }
    }
}

impl<B: 'static> ServiceRoute<B> {
    pub fn new<S>(service: S) -> Self
    where
        S: Service<ServerContext, Request<B>, Response = Response<BoxBody>, Error = Status>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let hook = service.stream_hook_factory().map(HookFactory);
        Self {
            inner: (
                BoxCloneService::new(service),
                S::descriptor(),
                S::NAME,
                hook,
                S::requires_managed_transport(),
            ),
            canonical_path: None,
            resolved_method: None,
        }
    }

    /// Specializes an existing service route for a directory's method binding.
    /// Resolve its static method description during registration, so dispatch
    /// can reuse the directory's selection without scanning the service again.
    pub fn with_canonical_path(mut self, path: impl Into<FastStr>) -> Self {
        let path = path.into();
        self.resolved_method = self.inner.1.and_then(|descriptor| {
            let (_, name) = path.rsplit_once('/')?;
            descriptor.methods.iter().find(|method| method.name == name)
        });
        self.canonical_path = Some(path);
        self
    }

    pub fn payload(&self) -> PayloadKind {
        self.inner
            .1
            .map(|descriptor| descriptor.payload)
            .unwrap_or_default()
    }

    pub fn name(&self) -> &'static str {
        self.inner.2
    }
}

/// The service/method facts remain owned by the caller's shared directory.
#[doc(hidden)]
pub trait RouteLookup<B = BoxBody>: Send + Sync {
    fn find(&self, path: &str) -> Result<ServiceRoute<B>, Status>;
    fn response_payload(&self, path: &str) -> Option<PayloadKind>;
    fn requires_managed_transport(&self) -> bool {
        true
    }
}

/// Preserves the received path when a directory rewrites it to a canonical codec path.
/// Absent when the received path already equals the native RPC context's method.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct OriginalGrpcPath(pub FastStr);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RouteId(u32);

impl RouteId {
    fn next() -> Self {
        // `AtomicU64` isn't supported on all platforms
        static ID: AtomicU32 = AtomicU32::new(0);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        if id == u32::MAX {
            panic!("Over `u32::MAX` routes created. If you need this, please file an issue.");
        }
        Self(id)
    }
}

#[derive(Default)]
pub struct Router<B = BoxBody> {
    routes: FxHashMap<RouteId, RoutedService<B>>,
    node: matchit::Router<RouteId>,
    stream_hook: Option<HookFactory>,
    pub(super) unsafe_codec: Option<bool>,
    shared: Option<Arc<dyn RouteLookup<B>>>,
}

impl<B> Clone for Router<B> {
    fn clone(&self) -> Self {
        Self {
            routes: self.routes.clone(),
            node: self.node.clone(),
            stream_hook: self.stream_hook.clone(),
            unsafe_codec: self.unsafe_codec,
            shared: self.shared.clone(),
        }
    }
}

impl<B> Router<B>
where
    B: HttpBody + 'static,
{
    pub fn new() -> Self {
        Self {
            routes: Default::default(),
            node: Default::default(),
            stream_hook: None,
            unsafe_codec: None,
            shared: None,
        }
    }

    /// Uses a lookup facade over one externally owned directory.
    #[doc(hidden)]
    pub fn from_lookup(lookup: Arc<dyn RouteLookup<B>>) -> Self {
        Self {
            shared: Some(lookup),
            ..Self::new()
        }
    }

    pub(super) fn attach_lookup(&mut self, lookup: Arc<dyn RouteLookup<B>>) -> std::io::Result<()> {
        if !self.routes.is_empty() || self.shared.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "register StreamX services in its shared directory",
            ));
        }
        self.shared = Some(lookup);
        Ok(())
    }

    pub fn add_service<S>(mut self, service: S) -> Self
    where
        S: Service<ServerContext, Request<B>, Response = Response<BoxBody>, Error = Status>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
    {
        assert!(
            self.shared.is_none(),
            "register services in the shared directory before constructing its gRPC view"
        );
        let path = format!("/{}/{{*rest}}", S::NAME);

        if path.is_empty() {
            panic!("[VOLO] Paths must start with a `/`. Use \"/\" for root routes");
        } else if !path.starts_with('/') {
            panic!("[VOLO] Paths must start with a `/`");
        }

        let id = RouteId::next();

        self.set_node(path, id);

        let hook = match (
            service.stream_hook_factory().map(HookFactory),
            self.stream_hook.clone(),
        ) {
            (Some(service), Some(default)) => Some(default.append(service)),
            (service, default) => service.or(default),
        };
        self.routes.insert(
            id,
            (
                BoxCloneService::new(service),
                S::descriptor(),
                S::NAME,
                hook,
                S::requires_managed_transport(),
            ),
        );

        self
    }

    pub(crate) fn with_stream_hook(mut self, next: HookFactory, front: bool) -> Self {
        for (_, _, _, hook, _) in self.routes.values_mut() {
            *hook = Some(match hook.take() {
                Some(previous) if front => next.clone().append(previous),
                Some(previous) => previous.append(next.clone()),
                None => next.clone(),
            });
        }
        self.stream_hook = Some(match self.stream_hook.take() {
            Some(previous) if front => next.append(previous),
            Some(previous) => previous.append(next),
            None => next,
        });
        self
    }

    pub(super) fn response_payloads(&self) -> Option<super::meta::ResponsePayloads> {
        if let Some(lookup) = self.shared.clone() {
            return Some(super::meta::ResponsePayloads::new(move |path| {
                lookup.response_payload(path)
            }));
        }
        if !self.routes.values().any(|(_, descriptor, _, _, _)| {
            descriptor.is_some_and(|d| d.payload == PayloadKind::Thrift)
        }) {
            return None;
        }
        let names: FxHashMap<_, _> = self
            .routes
            .values()
            .map(|(_, descriptor, name, _, _)| {
                (*name, descriptor.map(|d| d.payload).unwrap_or_default())
            })
            .collect();
        Some(super::meta::ResponsePayloads::new(move |path| {
            let service = path.strip_prefix('/')?.split_once('/')?.0;
            names.get(service).copied()
        }))
    }

    pub(crate) fn requires_managed_transport(&self) -> bool {
        if let Some(lookup) = &self.shared {
            return lookup.requires_managed_transport();
        }
        self.routes
            .values()
            .any(|(_, descriptor, _, hook, managed)| {
                hook.as_ref().is_some_and(|hook| match descriptor {
                    Some(descriptor) => descriptor
                        .methods
                        .iter()
                        .any(|method| hook.accepts(Some(method)).unwrap_or(true)),
                    None => hook.accepts(None).unwrap_or(true),
                }) || *managed
                    || descriptor
                        .is_some_and(|descriptor| descriptor.payload == PayloadKind::Thrift)
            })
    }

    #[track_caller]
    fn set_node(&mut self, path: String, id: RouteId) {
        if let Err(err) = self.node.insert(path, id) {
            panic!("[VOLO] Invalid route: {err}");
        }
    }
}

impl<B> Service<ServerContext, Request<B>> for Router<B>
where
    B: HttpBody + Send,
{
    type Response = Response<BoxBody>;
    type Error = Status;

    async fn call(
        &self,
        cx: &mut ServerContext,
        mut req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        cx.rpc_info.config_mut().set_unsafe_codec(self.unsafe_codec);
        let path = cx.rpc_info.method();
        let (binding, name) = if let Some(lookup) = &self.shared {
            (
                lookup.find(path)?,
                path.rsplit_once('/')
                    .map(|(_, name)| name)
                    .unwrap_or_default(),
            )
        } else {
            let matched = self
                .node
                .at(path)
                .map_err(|error| Status::unimplemented(error.to_string()))?;
            let inner = self.routes.get(matched.value).volo_unwrap().clone();
            (
                ServiceRoute {
                    inner,
                    canonical_path: None,
                    resolved_method: None,
                },
                matched.params.get("rest").unwrap_or_default(),
            )
        };
        let ServiceRoute {
            inner: (route, descriptor, _, hook, managed),
            canonical_path,
            resolved_method,
        } = binding;
        let hook = if self.shared.is_some() {
            match (hook, self.stream_hook.clone()) {
                (Some(service), Some(default)) => Some(default.append(service)),
                (service, default) => service.or(default),
            }
        } else {
            hook
        };
        let method = descriptor
            .map(|descriptor| {
                // Custom lookup implementations may return the same service
                // route for another method. Keep their original validation.
                resolved_method
                    .filter(|method| method.name == name)
                    .or_else(|| descriptor.methods.iter().find(|method| method.name == name))
                    .copied()
                    .ok_or_else(|| Status::unimplemented("unknown method"))
            })
            .transpose()?;
        let canonical_path = canonical_path.filter(|canonical| canonical != path);
        let original = canonical_path
            .as_ref()
            .map(|_| OriginalGrpcPath(path.clone()));
        if let Some(canonical) = canonical_path {
            cx.rpc_info.set_method(canonical);
        }
        if let Some(original) = &original {
            cx.extensions.insert(original.clone());
            req.extensions_mut().insert(original.clone());
        }
        let payload = descriptor.map(|d| d.payload).unwrap_or_default();
        if payload == PayloadKind::Thrift || cx.extensions.get::<ResponsePolicy>().is_some() {
            ResponsePolicy {
                payload,
                http_status: None,
            }
            .install(cx);
        }
        if let Some(method) = method {
            req.extensions_mut().insert(method);
            cx.extensions.insert(method);
            cx.extensions.insert(RegisteredRoute);
        }
        let strict = cx.extensions.get::<super::policy::StrictGrpc>().is_some();
        let hook = match hook.as_ref() {
            Some(hook) if hook.accepts(method.as_ref())? => Some(hook),
            _ => None,
        };
        if req
            .extensions()
            .get::<Deadline>()
            .copied()
            .is_some_and(Deadline::is_expired)
        {
            return Err(Status::deadline_exceeded("timeout"));
        }
        if strict
            || managed
            || descriptor.is_some_and(|descriptor| descriptor.payload == PayloadKind::Thrift)
            || hook.is_some()
        {
            // Allocate lifecycle state only for managed calls. This
            // keeps ordinary Protobuf request futures small.
            Box::pin(async move {
                let stream = StreamContext::new(
                    cx.rpc_info.method().clone(),
                    method.map(|method| method.mode),
                    volo::context::Role::Server,
                    req.extensions().get::<Deadline>().copied(),
                );
                stream.set_request_metadata(req.metadata_mut().snapshot());
                if let Some(owner) = req.extensions().get::<super::h2::StreamOwner>() {
                    owner.attach(
                        stream.clone(),
                        descriptor.map(|value| value.payload).unwrap_or_default(),
                    );
                }
                if let Some(scope) = req
                    .extensions()
                    .get::<Arc<dyn crate::streaming::StreamScope>>()
                {
                    stream.set_scope(scope.clone());
                }
                req.extensions_mut().insert(stream.clone());
                cx.extensions.insert(stream.clone());
                stream.with_extensions(|extensions| {
                    if let Some(method) = method {
                        extensions.insert(method);
                        extensions.insert(RegisteredRoute);
                    }
                    if let Some(original) = original {
                        extensions.insert(original);
                    }
                });
                let guard = CallGuard::new(stream.clone());
                if let Err(status) = stream.sync_scope(|| {
                    stream
                        .shared_context()
                        .open_hooks()
                        .map_err(Status::from)
                        .and_then(|()| stream.install_hook(hook))
                }) {
                    stream.finish(status.clone());
                    return Err(status);
                }
                stream.arm_deadline();
                let result = stream
                    .scope(async {
                        tokio::select! { biased;
                            status = stream.cancelled() => Err(status),
                            result = route.call(cx, req) => result,
                        }
                    })
                    .await;
                match result {
                    Ok(mut response) => {
                        // Compatibility for hand-written/old services without descriptors.
                        cx.extensions.insert(RegisteredRoute);
                        stream.with_extensions(|extensions| extensions.insert(RegisteredRoute));
                        response.extensions_mut().insert(stream);
                        Ok(response
                            .map(|body| crate::streaming::lifecycle_body::response(body, guard)))
                    }
                    Err(status) => {
                        stream.finish(status.clone());
                        Err(status)
                    }
                }
            })
            .await
        } else {
            let response = route.call(cx, req).await?;
            cx.extensions.insert(RegisteredRoute);
            Ok(response)
        }
    }
}

impl<B> fmt::Debug for Router<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router")
            .field("routes", &self.routes)
            .field("shared", &self.shared.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::{
        Code,
        message::{PayloadShape, RpcMode},
        streaming::{StreamHook, StreamStatus},
    };

    #[derive(Clone)]
    struct MethodProbe {
        calls: Arc<AtomicUsize>,
        original: Option<&'static str>,
    }
    impl NamedService for MethodProbe {
        const NAME: &'static str = "public.Probe";
        fn descriptor() -> Option<ServiceDescriptor> {
            Some(ServiceDescriptor {
                payload: PayloadKind::Thrift,
                methods: &[MethodDescriptor {
                    name: "Call",
                    mode: RpcMode::Unary,
                    payload: PayloadShape::UnaryArgsResult,
                }],
            })
        }
    }
    impl Service<ServerContext, Request<BoxBody>> for MethodProbe {
        type Response = Response<BoxBody>;
        type Error = Status;
        async fn call(
            &self,
            cx: &mut ServerContext,
            req: Request<BoxBody>,
        ) -> Result<Self::Response, Status> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!(cx.rpc_info.method(), "/public.Probe/Call");
            let method = req.extensions().get::<MethodDescriptor>().unwrap();
            assert_eq!(method.name, "Call");
            assert_eq!(method.mode, RpcMode::Unary);
            assert_eq!(method.payload, PayloadShape::UnaryArgsResult);
            assert_eq!(
                req.extensions()
                    .get::<OriginalGrpcPath>()
                    .map(|p| p.0.as_str()),
                self.original
            );
            assert_eq!(
                cx.extensions
                    .get::<OriginalGrpcPath>()
                    .map(|p| p.0.as_str()),
                self.original
            );
            let native = cx.extensions.get::<StreamContext>().unwrap();
            assert_eq!(native.method(), "/public.Probe/Call");
            assert_eq!(native.shared_context().route().unwrap().method, "Call");
            native.with_extensions(|extensions| {
                assert_eq!(
                    extensions.get::<OriginalGrpcPath>().map(|p| p.0.as_str()),
                    self.original
                );
            });
            Ok(Response::new(crate::body::empty_body()))
        }
    }

    struct FixedRoute(ServiceRoute);
    impl RouteLookup for FixedRoute {
        fn find(&self, _: &str) -> Result<ServiceRoute, Status> {
            Ok(self.0.clone())
        }
        fn response_payload(&self, _: &str) -> Option<PayloadKind> {
            Some(self.0.payload())
        }
    }

    #[tokio::test]
    async fn canonical_method_binding_preserves_alias_and_selected_method() {
        for (path, original) in [
            ("/public.Probe/Call", None),
            (
                "/other.package.Probe/Call",
                Some("/other.package.Probe/Call"),
            ),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let route = ServiceRoute::new(MethodProbe {
                calls: calls.clone(),
                original,
            })
            .with_canonical_path("/public.Probe/Call");
            let router = Router::from_lookup(Arc::new(FixedRoute(route)));
            let mut cx = ServerContext::default();
            cx.rpc_info.set_method(path.into());
            let mut req = Request::new(crate::body::empty_body());
            req.metadata_mut()
                .insert("content-type", "application/grpc+thrift".parse().unwrap());
            let response = router.call(&mut cx, req).await.unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            drop(response);
        }
    }

    #[tokio::test]
    async fn specialized_route_still_rejects_unknown_method_from_custom_lookup() {
        let calls = Arc::new(AtomicUsize::new(0));
        let route = ServiceRoute::new(MethodProbe {
            calls: calls.clone(),
            original: None,
        })
        .with_canonical_path("/public.Probe/Call");
        let router = Router::from_lookup(Arc::new(FixedRoute(route)));
        let mut cx = ServerContext::default();
        cx.rpc_info.set_method("/public.Probe/Missing".into());
        let error = router
            .call(&mut cx, Request::new(crate::body::empty_body()))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::Unimplemented);
        assert_eq!(error.message(), "unknown method");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(cx.extensions.get::<StreamContext>().is_none());
    }

    #[derive(Clone)]
    struct Probe(Arc<AtomicUsize>);
    impl NamedService for Probe {
        const NAME: &'static str = "public.Probe";
    }
    struct ProbeMessage;
    impl crate::message::RecvEntryMessage for ProbeMessage {
        fn from_body(
            _: Option<&str>,
            _: BoxBody,
            _: crate::codec::decode::Kind,
            _: Option<crate::codec::compression::CompressionEncoding>,
        ) -> Result<Self, Status> {
            panic!("invalid request must be rejected before decoding")
        }
    }
    impl crate::message::SendEntryMessage for ProbeMessage {
        fn into_body(
            self,
            _: Option<crate::codec::compression::CompressionEncoding>,
        ) -> crate::BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>> {
            panic!("invalid request must not produce a response message")
        }
    }
    impl Service<ServerContext, Request<ProbeMessage>> for Probe {
        type Response = Response<ProbeMessage>;
        type Error = Status;
        async fn call(
            &self,
            _: &mut ServerContext,
            _: Request<ProbeMessage>,
        ) -> Result<Self::Response, Status> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(Response::new(ProbeMessage))
        }
    }

    struct Observe {
        opens: Arc<AtomicUsize>,
        finishes: Arc<AtomicUsize>,
    }
    impl StreamHook for Observe {
        fn on_open(&self, _: &StreamContext) -> Result<(), Status> {
            self.opens.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn on_finish(&self, _: &StreamContext, status: &StreamStatus) {
            assert_eq!(status.status.code(), Code::Unimplemented);
            self.finishes.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn strict_policy_failure_opens_and_finishes_once_without_calling_the_service() {
        let calls = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let finishes = Arc::new(AtomicUsize::new(0));
        let observed_opens = opens.clone();
        let observed_finishes = finishes.clone();
        let router = Router::new()
            .add_service(
                super::super::ServiceBuilder::new(Probe(calls.clone()))
                    .build::<ProbeMessage, ProbeMessage>(),
            )
            .with_stream_hook(
                HookFactory(Arc::new(move |_: &StreamContext| Observe {
                    opens: observed_opens.clone(),
                    finishes: observed_finishes.clone(),
                })),
                false,
            );
        let mut cx = ServerContext::default();
        cx.rpc_info.set_method("/public.Probe/Call".into());
        cx.extensions.insert(super::super::policy::StrictGrpc);
        let mut request = Request::new(crate::body::empty_body());
        request
            .metadata_mut()
            .insert("content-type", "text/plain".parse().unwrap());
        let error = router.call(&mut cx, request).await.unwrap_err();
        assert_eq!(error.code(), Code::Unimplemented);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert_eq!(finishes.load(Ordering::Relaxed), 1);
        assert_eq!(
            cx.extensions.get::<ResponsePolicy>().unwrap().http_status,
            Some(http::StatusCode::UNSUPPORTED_MEDIA_TYPE)
        );
    }
}
