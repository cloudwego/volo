use std::{future::Future, pin::Pin, sync::Arc};

use rustc_hash::FxHashMap;

use crate::streaming::{OpenedStream, StreamContext, StreamError, StreamHandlerScope, StreamMode};

type Handler = Arc<
    dyn Fn(
            StreamContext,
            OpenedStream,
        ) -> Pin<Box<dyn Future<Output = Result<(), StreamError>> + Send>>
        + Send
        + Sync,
>;

/// A raw-stream handler plus its local typed/observability mode. Modes do not
/// change TTStream admission or validate the number of wire DATA frames.
#[derive(Clone)]
pub struct StreamMethod {
    pub(crate) handler: Handler,
    pub(crate) mode: StreamMode,
}
impl StreamMethod {
    pub fn new<F, Fut>(mode: StreamMode, handler: F) -> Self
    where
        F: Fn(StreamContext, OpenedStream) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), StreamError>> + Send + 'static,
    {
        Self {
            mode,
            handler: Arc::new(move |cx, stream| Box::pin(handler(cx, stream))),
        }
    }
}
type MethodLookup = Arc<dyn Fn(&str) -> Option<StreamMethod> + Send + Sync>;
#[derive(Clone)]
struct Generic {
    lookup: MethodLookup,
    searchable: bool,
}
#[derive(Clone)]
struct Service {
    name: String,
    // Unary descriptors count towards service/method lookup, but never cause
    // Args/Result envelopes to be reinterpreted as streaming struct payloads.
    methods: FxHashMap<String, Option<StreamMethod>>,
    generic: Option<Generic>,
}
impl Service {
    fn has_method(&self, method: &str, search: bool) -> bool {
        self.methods.contains_key(method) || self.generic_method(method, search).is_some()
    }
    fn generic_method(&self, method: &str, search: bool) -> Option<StreamMethod> {
        self.generic
            .as_ref()
            .filter(|generic| !search || generic.searchable)
            .and_then(|generic| (generic.lookup)(method))
    }
    fn method(&self, method: &str, unknown: &Option<StreamMethod>) -> Option<StreamMethod> {
        match self.methods.get(method) {
            Some(method) => method.clone(),
            None => self
                .generic_method(method, false)
                .or_else(|| unknown.clone()),
        }
    }
}

/// Kitex SearchService(strict=false) routing, independent of the wire driver.
/// Invalid combinations fail before listening (or before starting a raw connection).
#[derive(Clone, Default)]
pub struct StreamRouter {
    services: Vec<Service>,
    fallback: Option<String>,
    unknown: Option<StreamMethod>,
    special: Option<String>,
    refuse_without_service_name: bool,
    errors: Vec<String>,
    pub(crate) scopes: Vec<Arc<dyn StreamHandlerScope>>,
}
impl StreamRouter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn merge(&mut self, other: Self) {
        for service in other.services {
            if self.services.iter().any(|entry| entry.name == service.name) {
                self.errors
                    .push(format!("duplicate service registration: {}", service.name));
            }
            self.services.push(service);
        }
        self.scopes.extend(other.scopes);
        self.errors.extend(other.errors);
        if let Some(fallback) = other.fallback {
            self.fallback(fallback);
        }
        if let Some(unknown) = other.unknown {
            if self.unknown.replace(unknown).is_some() {
                self.errors.push("multiple unknown services".into());
            }
        }
        if let Some(special) = other.special {
            if self.special.replace(special).is_some() {
                self.errors
                    .push("multiple combine/binary-v1 services".into());
            }
        }
        self.refuse_without_service_name |= other.refuse_without_service_name;
    }
    fn service(&mut self, service: String) -> &mut Service {
        if !self.services.iter().any(|entry| entry.name == service) {
            self.services.push(Service {
                name: service.clone(),
                methods: FxHashMap::default(),
                generic: None,
            });
        }
        self.services
            .iter_mut()
            .find(|entry| entry.name == service)
            .expect("registered service")
    }
    /// Registers a service with no streaming methods, so mixed routing still
    /// knows the service even when an older generator has no method descriptors.
    pub fn declare_service(&mut self, service: impl Into<String>) {
        self.service(service.into());
    }
    pub fn add<F, Fut>(
        &mut self,
        service: impl Into<String>,
        method: impl Into<String>,
        handler: F,
    ) -> Result<(), StreamError>
    where
        F: Fn(StreamContext, OpenedStream) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), StreamError>> + Send + 'static,
    {
        self.add_mode(service, method, StreamMode::Raw, handler)
    }
    pub fn add_mode<F, Fut>(
        &mut self,
        service: impl Into<String>,
        method: impl Into<String>,
        mode: StreamMode,
        handler: F,
    ) -> Result<(), StreamError>
    where
        F: Fn(StreamContext, OpenedStream) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), StreamError>> + Send + 'static,
    {
        self.insert(
            service.into(),
            method.into(),
            Some(StreamMethod::new(mode, handler)),
        )
    }
    pub fn add_unary_method(
        &mut self,
        service: impl Into<String>,
        method: impl Into<String>,
    ) -> Result<(), StreamError> {
        self.insert(service.into(), method.into(), None)
    }
    fn insert(
        &mut self,
        service: String,
        method: String,
        handler: Option<StreamMethod>,
    ) -> Result<(), StreamError> {
        let entry = self.service(service);
        if entry.methods.contains_key(&method) {
            return Err(StreamError::configuration(format!(
                "duplicate method registration: {}/{}",
                entry.name, method
            )));
        }
        entry.methods.insert(method, handler);
        Ok(())
    }
    /// Dynamic JSON/map-style MethodInfo. Method lookup also participates in
    /// empty/wrong service-name searches, matching Kitex's generic metadata.
    pub fn add_generic(
        &mut self,
        service: impl Into<String>,
        lookup: impl Fn(&str) -> Option<StreamMethod> + Send + Sync + 'static,
    ) -> Result<(), StreamError> {
        self.generic(service.into(), Arc::new(lookup), true)
    }
    fn generic(
        &mut self,
        service: String,
        lookup: MethodLookup,
        searchable: bool,
    ) -> Result<(), StreamError> {
        let entry = self.service(service);
        if entry.generic.is_some() {
            return Err(StreamError::configuration(
                "duplicate generic method provider",
            ));
        }
        entry.generic = Some(Generic { lookup, searchable });
        Ok(())
    }
    /// BinaryThriftGenericV2 requires a known service name and accepts arbitrary
    /// method names. It is excluded from fallback-by-method searches.
    pub fn add_binary_generic(
        &mut self,
        service: impl Into<String>,
        method: StreamMethod,
    ) -> Result<(), StreamError> {
        self.generic(
            service.into(),
            Arc::new(move |_| Some(method.clone())),
            false,
        )
    }
    /// Compatibility route for Kitex's legacy $GenericService special case.
    pub fn binary_generic_v1(&mut self, method: StreamMethod) -> Result<(), StreamError> {
        self.add_binary_generic("$GenericService", method)?;
        self.combine_service("$GenericService")
    }
    /// A combine service ignores incoming service names; methods are still
    /// looked up normally. Like Kitex it cannot coexist with other services.
    pub fn combine_service(&mut self, service: impl Into<String>) -> Result<(), StreamError> {
        if self.special.is_some() {
            return Err(StreamError::configuration("multiple combine services"));
        }
        self.special = Some(service.into());
        Ok(())
    }
    /// Binary generic fallback for an unknown service or a missing method on a
    /// known service. No attacker-controlled service-name cache is retained.
    pub fn unknown_service(&mut self, method: StreamMethod) -> Result<(), StreamError> {
        if self.unknown.is_some() {
            return Err(StreamError::configuration("multiple unknown services"));
        }
        self.unknown = Some(method);
        Ok(())
    }
    pub fn refuse_traffic_without_service_name(&mut self, refuse: bool) {
        self.refuse_without_service_name = refuse;
    }
    pub fn fallback(&mut self, service: impl Into<String>) {
        let service = service.into();
        if self.fallback.is_some() {
            self.errors.push("multiple fallback services".into());
        }
        self.fallback = Some(service);
    }
    pub fn handler_scope(&mut self, scope: Arc<dyn StreamHandlerScope>) {
        self.scopes.push(scope);
    }
    pub fn has_streaming_methods(&self) -> bool {
        self.unknown.is_some()
            || self.services.iter().any(|service| {
                service.generic.is_some() || service.methods.values().any(Option::is_some)
            })
    }
    pub fn validate(&self) -> Result<(), StreamError> {
        if let Some(error) = self.errors.first() {
            return Err(StreamError::configuration(error));
        }
        if self.services.is_empty() && self.unknown.is_none() {
            return Err(StreamError::configuration(
                "no streaming services registered",
            ));
        }
        if let Some(name) = &self.fallback {
            if !self.services.iter().any(|service| &service.name == name) {
                return Err(StreamError::configuration(
                    "fallback service is not registered",
                ));
            }
        }
        if let Some(name) = &self.special {
            if self.services.len() != 1 || &self.services[0].name != name || self.unknown.is_some()
            {
                return Err(StreamError::configuration(
                    "combine/binary-v1 service must be the only service and cannot have unknownSvc",
                ));
            }
            return Ok(());
        }
        if self.refuse_without_service_name {
            return Ok(());
        }
        let fallback = self
            .services
            .iter()
            .find(|service| Some(&service.name) == self.fallback.as_ref());
        let mut counts = FxHashMap::<&str, usize>::default();
        for service in &self.services {
            for method in service.methods.keys() {
                let count = counts.entry(method.as_str()).or_default();
                *count += 1;
                if *count > 1
                    && !fallback.is_some_and(|service| service.methods.contains_key(method))
                {
                    return Err(StreamError::configuration(format!(
                        "method {method} conflicts across services without a fallback"
                    )));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn find(&self, service: &str, method: &str) -> Option<(String, StreamMethod)> {
        let selected = if let Some(name) = &self.special {
            self.services.iter().find(|entry| &entry.name == name)
        } else if self.refuse_without_service_name {
            self.services.iter().find(|entry| entry.name == service)
        } else if service.is_empty() {
            self.services
                .iter()
                .find(|entry| {
                    Some(&entry.name) == self.fallback.as_ref() && entry.has_method(method, true)
                })
                .or_else(|| {
                    self.services.iter().find(|entry| {
                        Some(&entry.name) != self.fallback.as_ref()
                            && entry.has_method(method, true)
                    })
                })
        } else if let Some(exact) = self.services.iter().find(|entry| entry.name == service) {
            Some(exact)
        } else {
            let mut matches = self
                .services
                .iter()
                .filter(|entry| entry.has_method(method, true));
            let first = matches.next();
            if matches.next().is_none() {
                first
            } else {
                None
            }
        };
        match selected {
            Some(service) => service
                .method(method, &self.unknown)
                .map(|method| (service.name.clone(), method)),
            None => self
                .unknown
                .clone()
                .map(|method| (service.to_owned(), method)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn method(mode: StreamMode) -> StreamMethod {
        StreamMethod::new(mode, |_, _| async { Ok(()) })
    }
    fn add(router: &mut StreamRouter, service: &str, name: &str, mode: StreamMode) {
        router
            .add_mode(service, name, mode, |_, _| async { Ok(()) })
            .unwrap();
    }
    fn mode(router: &StreamRouter, service: &str, name: &str) -> Option<StreamMode> {
        router.find(service, name).map(|(_, method)| method.mode)
    }
    #[test]
    fn service_and_method_lookup_matches_non_strict_rules() {
        let mut router = StreamRouter::new();
        add(&mut router, "A", "one", StreamMode::Client);
        add(&mut router, "B", "two", StreamMode::Server);
        router.validate().unwrap();
        for service in ["A", "", "wrong"] {
            assert_eq!(mode(&router, service, "one"), Some(StreamMode::Client));
        }
        // Exact known service wins even if a different service contains the method.
        assert_eq!(mode(&router, "B", "one"), None);
        assert_eq!(mode(&router, "missing", "none"), None);
        add(&mut router, "A", "common", StreamMode::Client);
        add(&mut router, "B", "common", StreamMode::Server);
        assert!(router.validate().is_err());
        router.fallback("B");
        router.validate().unwrap();
        assert_eq!(mode(&router, "", "common"), Some(StreamMode::Server));
        assert_eq!(mode(&router, "wrong", "common"), None);
        assert_eq!(mode(&router, "A", "common"), Some(StreamMode::Client));
        router.unknown_service(method(StreamMode::Raw)).unwrap();
        assert_eq!(mode(&router, "wrong", "common"), Some(StreamMode::Raw));
        assert_eq!(mode(&router, "A", "unknown"), Some(StreamMode::Raw));
        assert!(router.unknown_service(method(StreamMode::Raw)).is_err());
    }
    #[test]
    fn generic_search_and_binary_v2_are_distinct() {
        let mut router = StreamRouter::new();
        router
            .add_binary_generic("Binary", method(StreamMode::Bidirectional))
            .unwrap();
        router
            .add_generic("Json", |name| {
                (name == "dynamic").then(|| method(StreamMode::Server))
            })
            .unwrap();
        router.validate().unwrap();
        assert_eq!(
            mode(&router, "Binary", "anything"),
            Some(StreamMode::Bidirectional)
        );
        assert_eq!(mode(&router, "wrong", "anything"), None);
        assert_eq!(mode(&router, "", "anything"), None);
        for service in ["Json", "", "wrong"] {
            assert_eq!(mode(&router, service, "dynamic"), Some(StreamMode::Server));
        }
        router.fallback("Binary");
        assert_eq!(mode(&router, "", "dynamic"), Some(StreamMode::Server));
        assert_eq!(mode(&router, "", "anything"), None);
    }
    #[test]
    fn strict_configuration_and_special_services_match_kitex() {
        let mut strict = StreamRouter::new();
        add(&mut strict, "A", "common", StreamMode::Client);
        add(&mut strict, "B", "common", StreamMode::Server);
        strict.refuse_traffic_without_service_name(true);
        strict.validate().unwrap();
        assert_eq!(mode(&strict, "", "common"), None);
        assert_eq!(mode(&strict, "wrong", "common"), None);
        strict.unknown_service(method(StreamMode::Raw)).unwrap();
        assert_eq!(mode(&strict, "", "common"), Some(StreamMode::Raw));
        let mut combine = StreamRouter::new();
        add(&mut combine, "Combined", "call", StreamMode::Client);
        combine.combine_service("Combined").unwrap();
        combine.refuse_traffic_without_service_name(true);
        combine.validate().unwrap();
        assert_eq!(mode(&combine, "wrong", "call"), Some(StreamMode::Client));
        combine.unknown_service(method(StreamMode::Raw)).unwrap();
        assert!(combine.validate().is_err());
        let mut binary = StreamRouter::new();
        binary
            .binary_generic_v1(method(StreamMode::Bidirectional))
            .unwrap();
        binary.validate().unwrap();
        assert_eq!(
            mode(&binary, "wrong", "anything"),
            Some(StreamMode::Bidirectional)
        );
        add(&mut binary, "Other", "x", StreamMode::Client);
        assert!(binary.validate().is_err());
    }
    #[test]
    fn unary_descriptors_participate_without_changing_payload_adapters() {
        let mut router = StreamRouter::new();
        router.add_unary_method("Unary", "same").unwrap();
        add(&mut router, "Stream", "same", StreamMode::Client);
        assert!(router.validate().is_err());
        router.fallback("Unary");
        router.unknown_service(method(StreamMode::Raw)).unwrap();
        router.validate().unwrap();
        assert_eq!(mode(&router, "", "same"), None);
        assert_eq!(mode(&router, "Unary", "same"), None);
        assert_eq!(mode(&router, "wrong", "same"), Some(StreamMode::Raw));
        assert_eq!(mode(&router, "Stream", "same"), Some(StreamMode::Client));
    }
    #[test]
    fn registration_errors_survive_merge_and_fail_before_admission() {
        let mut first = StreamRouter::new();
        add(&mut first, "A", "one", StreamMode::Client);
        let mut second = StreamRouter::new();
        add(&mut second, "A", "two", StreamMode::Server);
        first.merge(second);
        assert!(first.validate().is_err());
        let mut fallback = StreamRouter::new();
        add(&mut fallback, "A", "one", StreamMode::Client);
        fallback.fallback("missing");
        assert!(fallback.validate().is_err());
        fallback.fallback("A");
        assert!(fallback.validate().is_err());
        assert!(StreamRouter::new().validate().is_err());
    }
}
