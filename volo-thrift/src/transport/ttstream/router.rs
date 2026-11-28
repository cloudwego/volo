use std::{future::Future, pin::Pin, sync::Arc};

use volo::streaming::ServiceDirectory;

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

/// A read-only TTStream projection of a shared service directory.
#[doc(hidden)]
pub trait StreamLookup: Send + Sync {
    fn find(&self, service: &str, method: &str) -> Option<(&str, StreamMethod)>;
    fn validate(&self) -> Result<(), StreamError>;
    fn has_streaming_methods(&self) -> bool;
}

#[doc(hidden)]
pub type StreamDeclarations = (
    ServiceDirectory<Option<StreamMethod>>,
    Vec<Arc<dyn StreamHandlerScope>>,
);

/// TTStream bindings over the shared transport-independent service directory.
#[derive(Clone, Default)]
pub struct StreamRouter {
    directory: ServiceDirectory<Option<StreamMethod>>,
    shared: Option<Arc<dyn StreamLookup>>,
    pub(crate) scopes: Vec<Arc<dyn StreamHandlerScope>>,
}

impl StreamRouter {
    pub fn new() -> Self {
        Self::default()
    }

    #[doc(hidden)]
    pub fn from_lookup(lookup: Arc<dyn StreamLookup>) -> Self {
        Self {
            shared: Some(lookup),
            ..Self::new()
        }
    }

    /// Transfers generated declarations into the common registration phase.
    #[doc(hidden)]
    pub fn into_parts(self) -> Result<StreamDeclarations, StreamError> {
        if self.shared.is_some() {
            return Err(StreamError::Configuration(
                "cannot extract declarations from a shared TTStream view".into(),
            ));
        }
        Ok((self.directory, self.scopes))
    }

    fn directory_mut(&mut self) -> &mut ServiceDirectory<Option<StreamMethod>> {
        assert!(
            self.shared.is_none(),
            "register services in the shared directory before constructing its TTStream view"
        );
        &mut self.directory
    }

    pub fn merge(&mut self, other: Self) {
        assert!(
            other.shared.is_none(),
            "cannot merge a shared TTStream view"
        );
        self.directory_mut().merge(other.directory);
        self.scopes.extend(other.scopes);
    }
    pub fn declare_service(&mut self, service: impl Into<String>) {
        self.directory_mut().declare_service(service);
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
        let binding = StreamMethod::new(mode, handler);
        self.directory_mut()
            .add_method(service, method, Some(binding))
            .map_err(Into::into)
    }
    pub fn add_unary_method(
        &mut self,
        service: impl Into<String>,
        method: impl Into<String>,
    ) -> Result<(), StreamError> {
        self.directory_mut()
            .add_method(service, method, None)
            .map_err(Into::into)
    }
    pub fn combine_service(&mut self, service: impl Into<String>) -> Result<(), StreamError> {
        self.directory_mut()
            .combine_service(service)
            .map_err(Into::into)
    }
    pub fn refuse_traffic_without_service_name(&mut self, refuse: bool) {
        self.directory_mut()
            .refuse_traffic_without_service_name(refuse);
    }
    pub fn fallback(&mut self, service: impl Into<String>) {
        self.directory_mut().fallback(service);
    }
    pub fn handler_scope(&mut self, scope: Arc<dyn StreamHandlerScope>) {
        self.scopes.push(scope);
    }
    pub fn has_streaming_methods(&self) -> bool {
        if let Some(lookup) = &self.shared {
            return lookup.has_streaming_methods();
        }
        self.directory
            .services()
            .any(|service| service.methods().any(|(_, method)| method.is_some()))
    }
    pub fn validate(&self) -> Result<(), StreamError> {
        if let Some(lookup) = &self.shared {
            return lookup.validate();
        }
        self.directory.validate().map_err(Into::into)
    }
    pub(crate) fn find(&self, service: &str, method: &str) -> Option<(&str, StreamMethod)> {
        if let Some(lookup) = &self.shared {
            return lookup.find(service, method);
        }
        let (service, method) = self.directory.find(service, method)?;
        method.map(|method| (service, method))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut combine = StreamRouter::new();
        add(&mut combine, "Combined", "call", StreamMode::Client);
        combine.combine_service("Combined").unwrap();
        combine.refuse_traffic_without_service_name(true);
        combine.validate().unwrap();
        assert_eq!(mode(&combine, "wrong", "call"), None);
        assert_eq!(mode(&combine, "Combined", "call"), Some(StreamMode::Client));
    }
    #[test]
    fn unary_descriptors_participate_without_changing_payload_adapters() {
        let mut router = StreamRouter::new();
        router.add_unary_method("Unary", "same").unwrap();
        add(&mut router, "Stream", "same", StreamMode::Client);
        assert!(router.validate().is_err());
        router.fallback("Unary");
        router.validate().unwrap();
        assert_eq!(mode(&router, "", "same"), None);
        assert_eq!(mode(&router, "Unary", "same"), None);
        assert_eq!(mode(&router, "wrong", "same"), None);
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
