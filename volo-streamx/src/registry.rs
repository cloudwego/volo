use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub use volo::streaming::RegistrationError;
use volo::streaming::ServiceDirectory;
use volo_grpc::{
    Status,
    server::{RouteLookup, ServiceRoute},
};
use volo_thrift::{
    streaming::StreamError,
    transport::ttstream::router::{StreamLookup, StreamMethod, StreamRouter},
};

use crate::{MethodDescriptor, PayloadKind, ServiceDescriptor};

fn invalid(message: impl Into<String>) -> RegistrationError {
    RegistrationError::new(message)
}

/// Implemented by generated BoundService values sharing one business instance.
pub trait Binding: Send + Sync + 'static {
    fn registration(&self) -> ServiceRegistration;
}

/// Generated declarations and protocol bindings for one logical service.
/// Add this once, after configuring its native codecs and hooks.
#[doc(hidden)]
pub struct ServiceRegistration {
    descriptor: &'static ServiceDescriptor,
    grpc: Option<ServiceRoute>,
    ttstream: Option<StreamRouter>,
    unary: Option<Arc<dyn volo_thrift::server::routed::UnaryBinding>>,
}

impl ServiceRegistration {
    pub(crate) fn name(&self) -> &'static str {
        self.descriptor.name
    }
    pub fn new(descriptor: &'static ServiceDescriptor) -> Self {
        Self {
            descriptor,
            grpc: None,
            ttstream: None,
            unary: None,
        }
    }

    pub fn with_grpc(mut self, route: ServiceRoute) -> Self {
        self.grpc = Some(route);
        self
    }

    pub fn with_thrift<S, Req>(mut self, service: S) -> Self
    where
        S: volo::Service<
                volo_thrift::context::ServerContext,
                Req,
                Error = volo_thrift::ServerError,
            > + Send
            + Sync
            + 'static,
        Req: volo_thrift::EntryMessage + 'static,
        S::Response: volo_thrift::EntryMessage + 'static,
    {
        self.unary = Some(volo_thrift::server::routed::bind(service));
        self
    }

    /// Only generated declarations belong here; set lookup policy on Registry.
    pub fn with_ttstream(mut self, router: StreamRouter) -> Self {
        self.ttstream = Some(router);
        self
    }
}

/// One selected method. Capabilities are checked after service selection.
pub struct MethodBinding {
    service: &'static ServiceDescriptor,
    method: &'static MethodDescriptor,
    grpc: Option<ServiceRoute>,
    ttstream: Option<StreamMethod>,
    unary: Option<Arc<dyn volo_thrift::server::routed::UnaryBinding>>,
}

impl MethodBinding {
    pub fn service(&self) -> &'static ServiceDescriptor {
        self.service
    }
    pub fn method(&self) -> &'static MethodDescriptor {
        self.method
    }

    fn protocols(&self) -> crate::Protocols {
        let mut protocols = crate::Protocols::NONE;
        if self.unary.is_some() {
            protocols = protocols | crate::Protocols::THRIFT;
        }
        if self.ttstream.is_some() {
            protocols = protocols | crate::Protocols::TTSTREAM;
        }
        if self.grpc.is_some() {
            protocols = protocols | crate::Protocols::GRPC;
        }
        protocols
    }
}

/// The only mutable registration phase; native routers receive read-only views.
#[derive(Default)]
pub struct Registry {
    directory: ServiceDirectory<Arc<MethodBinding>>,
    descriptors: Vec<&'static ServiceDescriptor>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, binding: impl Binding) -> Result<(), RegistrationError> {
        self.add(binding.registration())
    }

    pub fn add(&mut self, registration: ServiceRegistration) -> Result<(), RegistrationError> {
        let ServiceRegistration {
            descriptor,
            grpc,
            ttstream,
            unary,
        } = registration;
        if unary.is_some() && descriptor.payload != PayloadKind::Thrift {
            return Err(invalid(
                "Protobuf services cannot have a Thrift unary binding",
            ));
        }
        if descriptor.name.is_empty()
            || descriptor.name.contains(['.', '/'])
            || descriptor.wire_name.is_empty()
            || descriptor.wire_name.contains('/')
        {
            return Err(invalid(format!(
                "invalid service/wire name: {}/{}",
                descriptor.name, descriptor.wire_name
            )));
        }
        if let Some(previous) = self.descriptors.iter().find(|d| d.name == descriptor.name) {
            return Err(invalid(format!(
                "duplicate service short name {}: {} ({:?}) and {} ({:?})",
                descriptor.name,
                previous.wire_name,
                previous.payload,
                descriptor.wire_name,
                descriptor.payload
            )));
        }
        if let Some(previous) = self
            .descriptors
            .iter()
            .find(|d| d.wire_name == descriptor.wire_name)
        {
            return Err(invalid(format!(
                "duplicate wire service name {}: {} and {}",
                descriptor.wire_name, previous.name, descriptor.name
            )));
        }
        let mut methods = HashSet::new();
        for method in descriptor.methods {
            if method.name.is_empty() || method.name.contains('/') || !methods.insert(method.name) {
                return Err(invalid(format!(
                    "invalid/duplicate method: {}/{}",
                    descriptor.name, method.name
                )));
            }
            if method.oneway
                && (descriptor.payload != PayloadKind::Thrift
                    || method.mode != crate::RpcMode::Unary)
            {
                return Err(invalid(format!(
                    "invalid oneway method: {}/{}",
                    descriptor.name, method.name
                )));
            }
        }
        if let Some(route) = &grpc {
            let expected = match descriptor.payload {
                PayloadKind::Thrift => volo_grpc::message::PayloadKind::Thrift,
                PayloadKind::Protobuf => volo_grpc::message::PayloadKind::Protobuf,
            };
            if route.name() != descriptor.wire_name || route.payload() != expected {
                return Err(invalid(format!(
                    "gRPC binding does not match descriptor for {}",
                    descriptor.name
                )));
            }
        }
        let streams = match ttstream {
            Some(router) => {
                if descriptor.payload != PayloadKind::Thrift {
                    return Err(invalid("Protobuf services cannot have a TTStream binding"));
                }
                let (directory, scopes) =
                    router.into_parts().map_err(|e| invalid(e.to_string()))?;
                if directory.has_routing_policy() || !scopes.is_empty() {
                    return Err(invalid(
                        "TTStream declaration source contains routing policy or handler scopes; \
                         configure these on the shared server",
                    ));
                }
                for service in directory.services() {
                    if service.name() != descriptor.name
                        || service.methods().any(|(name, _)| !methods.contains(name))
                    {
                        return Err(invalid(format!(
                            "TTStream declarations do not match {}",
                            descriptor.name
                        )));
                    }
                }
                if directory.services().next().is_some() {
                    directory.validate().map_err(|e| invalid(e.to_string()))?;
                }
                Some(directory)
            }
            None => None,
        };
        let records = descriptor
            .methods
            .iter()
            .map(|method| {
                let ttstream = streams
                    .as_ref()
                    .and_then(|streams| streams.find_strict(descriptor.name, method.name))
                    .and_then(|(_, binding)| binding);
                if ttstream.is_some() && (method.mode == crate::RpcMode::Unary || method.oneway) {
                    return Err(invalid(format!(
                        "unary/oneway method has a TTStream binding: {}/{}",
                        descriptor.name, method.name
                    )));
                }
                Ok((
                    method.name,
                    Arc::new(MethodBinding {
                        service: descriptor,
                        method,
                        grpc: (!method.oneway)
                            .then(|| {
                                grpc.clone().map(|route| {
                                    route.with_canonical_path(format!(
                                        "/{}/{}",
                                        descriptor.wire_name, method.name
                                    ))
                                })
                            })
                            .flatten(),
                        ttstream,
                        unary: (method.mode == crate::RpcMode::Unary || method.oneway)
                            .then(|| unary.clone())
                            .flatten(),
                    }),
                ))
            })
            .collect::<Result<Vec<_>, RegistrationError>>()?;
        self.directory.declare_service(descriptor.name);
        for (name, record) in records {
            self.directory
                .add_method(descriptor.name, name, record)
                .map_err(|e| invalid(e.to_string()))?;
        }
        self.descriptors.push(descriptor);
        Ok(())
    }

    /// Mark an already registered Thrift service as Kitex's combine service.
    pub fn combine_service(&mut self, service: &str) -> Result<(), RegistrationError> {
        if !self
            .descriptors
            .iter()
            .any(|d| d.name == service && d.payload == PayloadKind::Thrift)
        {
            return Err(invalid("combine requires a registered Thrift service"));
        }
        self.directory
            .combine_service(service)
            .map_err(|e| invalid(e.to_string()))
    }

    pub fn fallback(&mut self, service: impl Into<String>) {
        self.directory.fallback(service);
    }

    pub fn refuse_traffic_without_service_name(&mut self, refuse: bool) {
        self.directory.refuse_traffic_without_service_name(refuse);
    }

    pub fn freeze(self) -> Result<Arc<FrozenRegistry>, RegistrationError> {
        self.directory
            .validate()
            .map_err(|e| invalid(e.to_string()))?;
        let names = self
            .descriptors
            .into_iter()
            .map(|d| (d.name, d))
            .collect::<HashMap<_, _>>();
        let wire_names = names.values().map(|d| (d.wire_name, d.name)).collect();
        Ok(Arc::new(FrozenRegistry {
            directory: self.directory,
            names,
            wire_names,
        }))
    }
}

/// Immutable directory and derived wire-name index, shared by every driver.
pub struct FrozenRegistry {
    directory: ServiceDirectory<Arc<MethodBinding>>,
    names: HashMap<&'static str, &'static ServiceDescriptor>,
    wire_names: HashMap<&'static str, &'static str>,
}

impl FrozenRegistry {
    pub(crate) fn protocols(&self) -> crate::Protocols {
        let mut protocols = crate::Protocols::NONE;
        for service in self.directory.services() {
            for (_, method) in service.methods() {
                protocols = protocols | method.protocols();
            }
        }
        protocols
    }
    #[doc(hidden)]
    pub fn thrift_service(&self) -> volo_thrift::server::routed::RoutedService {
        volo_thrift::server::routed::RoutedService
    }

    #[doc(hidden)]
    pub fn thrift_codec<M>(
        self: &Arc<Self>,
        codec: M,
    ) -> volo_thrift::server::routed::RoutedMakeCodec<M> {
        volo_thrift::server::routed::RoutedMakeCodec::new(codec, self.clone())
    }
    pub fn grpc_router(self: &Arc<Self>) -> volo_grpc::server::Router {
        volo_grpc::server::Router::from_lookup(self.clone())
    }

    pub fn ttstream_router(self: &Arc<Self>) -> StreamRouter {
        StreamRouter::from_lookup(self.clone())
    }

    pub fn find_thrift(&self, service: &str, method: &str) -> Option<Arc<MethodBinding>> {
        self.directory
            .find(service, method)
            .map(|(_, binding)| binding)
    }

    fn path<'a>(&'a self, path: &'a str) -> Option<(&'a str, &'a str)> {
        let (wire, method) = path.strip_prefix('/')?.split_once('/')?;
        if wire.is_empty() || method.is_empty() || method.contains('/') {
            return None;
        }
        let name = match self.wire_names.get(wire).copied() {
            Some(name) => name,
            None => {
                let name = wire.rsplit('.').next()?;
                // Only Thrift has the legacy Kitex short-service alias. Protobuf
                // packages are part of the service's identity on the wire.
                if self.names.get(name)?.payload != PayloadKind::Thrift {
                    return None;
                }
                name
            }
        };
        Some((name, method))
    }
}

impl volo_thrift::server::routed::UnaryLookup for FrozenRegistry {
    fn find(
        &self,
        service: &str,
        method: &str,
    ) -> Option<Arc<dyn volo_thrift::server::routed::UnaryBinding>> {
        self.find_thrift(service, method)?.unary.clone()
    }
}

impl RouteLookup for FrozenRegistry {
    fn find(&self, path: &str) -> Result<ServiceRoute, Status> {
        let (service, method) = self
            .path(path)
            .ok_or_else(|| Status::unimplemented("invalid RPC path"))?;
        let (_, binding) = self
            .directory
            .find_strict(service, method)
            .ok_or_else(|| Status::unimplemented("unknown service or method"))?;
        binding
            .grpc
            .clone()
            .ok_or_else(|| Status::unimplemented("method does not support gRPC"))
    }

    fn response_payload(&self, path: &str) -> Option<volo_grpc::message::PayloadKind> {
        let (service, _) = self.path(path)?;
        let descriptor = self.names.get(service)?;
        Some(match descriptor.payload {
            PayloadKind::Thrift => volo_grpc::message::PayloadKind::Thrift,
            PayloadKind::Protobuf => volo_grpc::message::PayloadKind::Protobuf,
        })
    }
}

impl StreamLookup for FrozenRegistry {
    fn find(&self, service: &str, method: &str) -> Option<(&str, StreamMethod)> {
        let (service, binding) = self.directory.find(service, method)?;
        binding.ttstream.clone().map(|method| (service, method))
    }

    fn validate(&self) -> Result<(), StreamError> {
        self.directory.validate().map_err(Into::into)
    }

    fn has_streaming_methods(&self) -> bool {
        self.protocols().contains(crate::Protocol::Ttstream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RpcMode;

    static A: ServiceDescriptor = ServiceDescriptor {
        name: "A",
        wire_name: "alpha.A",
        payload: PayloadKind::Thrift,
        methods: &[
            MethodDescriptor {
                name: "Shared",
                mode: RpcMode::Unary,
                oneway: false,
            },
            MethodDescriptor {
                name: "OnlyA",
                mode: RpcMode::Unary,
                oneway: false,
            },
        ],
    };
    static B: ServiceDescriptor = ServiceDescriptor {
        name: "B",
        wire_name: "beta.B",
        payload: PayloadKind::Protobuf,
        methods: &[MethodDescriptor {
            name: "Shared",
            mode: RpcMode::Unary,
            oneway: false,
        }],
    };

    fn add(registry: &mut Registry, descriptor: &'static ServiceDescriptor) {
        registry.add(ServiceRegistration::new(descriptor)).unwrap();
    }

    #[test]
    fn protobuf_wire_names_do_not_fall_back_to_a_different_package() {
        let mut registry = Registry::new();
        add(&mut registry, &B);
        let registry = registry.freeze().unwrap();
        assert_eq!(registry.path("/beta.B/Shared"), Some(("B", "Shared")));
        for path in ["/other.B/Shared", "/B/Shared"] {
            assert_eq!(registry.path(path), None, "{path}");
            assert_eq!(
                RouteLookup::find(&*registry, path).err().unwrap().code(),
                volo_grpc::Code::Unimplemented
            );
        }
    }

    #[test]
    fn one_directory_keeps_non_strict_selection_across_strict_h2_queries() {
        let mut registry = Registry::new();
        add(&mut registry, &A);
        add(&mut registry, &B);
        registry.fallback("B");
        let registry = registry.freeze().unwrap();
        let fallback = registry.find_thrift("", "Shared").unwrap();
        assert_eq!(fallback.service().name, "B");
        assert!(registry.find_thrift("B", "OnlyA").is_none());
        assert!(registry.find_thrift("wrong", "Shared").is_none());
        let unique = registry.find_thrift("wrong", "OnlyA").unwrap();
        assert_eq!(unique.service().name, "A");
        for _ in 0..4 {
            assert!(RouteLookup::find(&*registry, "/wrong/OnlyA").is_err());
            assert!(RouteLookup::find(&*registry, "/B/OnlyA").is_err());
        }
        assert!(Arc::ptr_eq(
            &fallback,
            &registry.find_thrift("", "Shared").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &unique,
            &registry.find_thrift("wrong", "OnlyA").unwrap()
        ));
        assert_eq!(
            RouteLookup::response_payload(&*registry, "/other.package.A/Missing"),
            Some(volo_grpc::message::PayloadKind::Thrift)
        );
        assert_eq!(
            RouteLookup::response_payload(&*registry, "/wrong/Missing"),
            None
        );
    }

    #[test]
    fn cross_payload_conflicts_need_a_covering_fallback_or_explicit_strict_policy() {
        let mut registry = Registry::new();
        add(&mut registry, &A);
        add(&mut registry, &B);
        assert!(
            registry
                .freeze()
                .err()
                .unwrap()
                .to_string()
                .contains("Shared")
        );
        let mut registry = Registry::new();
        add(&mut registry, &A);
        add(&mut registry, &B);
        registry.refuse_traffic_without_service_name(true);
        let registry = registry.freeze().unwrap();
        assert!(registry.find_thrift("", "Shared").is_none());
        assert!(registry.find_thrift("wrong", "OnlyA").is_none());
        assert_eq!(
            registry.find_thrift("A", "OnlyA").unwrap().service().name,
            "A"
        );
    }

    #[test]
    fn short_name_collisions_are_rejected_across_package_and_payload_without_partial_registration()
    {
        static SAME_PROTO: ServiceDescriptor = ServiceDescriptor {
            name: "A",
            wire_name: "beta.A",
            payload: PayloadKind::Protobuf,
            methods: &[],
        };
        static SAME_THRIFT: ServiceDescriptor = ServiceDescriptor {
            name: "A",
            wire_name: "gamma.A",
            payload: PayloadKind::Thrift,
            methods: &[],
        };
        for descriptor in [&SAME_PROTO, &SAME_THRIFT] {
            let mut registry = Registry::new();
            add(&mut registry, &A);
            let error = registry
                .add(ServiceRegistration::new(descriptor))
                .unwrap_err()
                .to_string();
            assert!(error.contains("alpha.A"));
            assert!(error.contains(descriptor.wire_name));
            assert_eq!(
                registry
                    .freeze()
                    .unwrap()
                    .find_thrift("A", "OnlyA")
                    .unwrap()
                    .service()
                    .wire_name,
                "alpha.A"
            );
        }
    }

    #[test]
    fn invalid_declarations_do_not_poison_corrected_registration() {
        static DUPLICATE: ServiceDescriptor = ServiceDescriptor {
            name: "A",
            wire_name: "alpha.A",
            payload: PayloadKind::Thrift,
            methods: &[
                MethodDescriptor {
                    name: "Shared",
                    mode: RpcMode::Unary,
                    oneway: false,
                },
                MethodDescriptor {
                    name: "Shared",
                    mode: RpcMode::Unary,
                    oneway: false,
                },
            ],
        };
        let mut registry = Registry::new();
        assert!(registry.add(ServiceRegistration::new(&DUPLICATE)).is_err());
        add(&mut registry, &A);
        assert_eq!(
            registry
                .freeze()
                .unwrap()
                .find_thrift("A", "OnlyA")
                .unwrap()
                .method()
                .name,
            "OnlyA"
        );
    }

    #[test]
    fn ttstream_checks_capability_after_shared_service_selection() {
        static STREAM: ServiceDescriptor = ServiceDescriptor {
            name: "Stream",
            wire_name: "public.Stream",
            payload: PayloadKind::Thrift,
            methods: &[MethodDescriptor {
                name: "Shared",
                mode: RpcMode::ClientStreaming,
                oneway: false,
            }],
        };
        let mut streams = StreamRouter::new();
        streams
            .add_mode(
                "Stream",
                "Shared",
                volo_thrift::streaming::StreamMode::Client,
                |_, _| async { Ok(()) },
            )
            .unwrap();
        let mut registry = Registry::new();
        registry
            .add(ServiceRegistration::new(&STREAM).with_ttstream(streams))
            .unwrap();
        add(&mut registry, &B);
        registry.fallback("B");
        let registry = registry.freeze().unwrap();
        assert!(StreamLookup::find(&*registry, "Stream", "Shared").is_some());
        assert!(
            StreamLookup::find(&*registry, "", "Shared").is_none(),
            "Protobuf fallback must win service selection before the TTStream capability check"
        );
        assert!(
            StreamLookup::find(&*registry, "wrong", "Shared").is_none(),
            "the Protobuf candidate must still participate in ambiguity detection"
        );
        assert_eq!(
            registry.find_thrift("", "Shared").unwrap().service().name,
            "B"
        );
    }
}
