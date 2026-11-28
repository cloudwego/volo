use rustc_hash::FxHashMap;

#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct RegistrationError(String);

impl RegistrationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// One service's complete method records, shared by all of its transports.
#[derive(Clone)]
pub struct ServiceEntry<M> {
    name: String,
    methods: FxHashMap<String, M>,
}

impl<M> ServiceEntry<M> {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn methods(&self) -> impl Iterator<Item = (&str, &M)> {
        self.methods
            .iter()
            .map(|(name, record)| (name.as_str(), record))
    }
}

/// Kitex service selection extracted from the TTStream router.
///
/// A record contains the capabilities and bindings for one method. Selection
/// always happens before the caller checks a record's payload or transport.
#[derive(Clone)]
pub struct ServiceDirectory<M> {
    services: Vec<ServiceEntry<M>>,
    names: FxHashMap<String, usize>,
    fallback: Option<String>,
    special: Option<String>,
    refuse_without_service_name: bool,
    errors: Vec<RegistrationError>,
}

impl<M> Default for ServiceDirectory<M> {
    fn default() -> Self {
        Self {
            services: Vec::new(),
            names: FxHashMap::default(),
            fallback: None,
            special: None,
            refuse_without_service_name: false,
            errors: Vec::new(),
        }
    }
}

impl<M> ServiceDirectory<M> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn services(&self) -> impl Iterator<Item = &ServiceEntry<M>> {
        self.services.iter()
    }

    /// Whether a declaration source also configures service selection policy.
    pub fn has_routing_policy(&self) -> bool {
        self.fallback.is_some() || self.special.is_some() || self.refuse_without_service_name
    }

    pub fn merge(&mut self, other: Self) {
        for service in other.services {
            if self.services.iter().any(|entry| entry.name == service.name) {
                self.errors.push(RegistrationError::new(format!(
                    "duplicate service registration: {}",
                    service.name
                )));
            }
            self.names
                .entry(service.name.clone())
                .or_insert(self.services.len());
            self.services.push(service);
        }
        self.errors.extend(other.errors);
        if let Some(fallback) = other.fallback {
            self.fallback(fallback);
        }
        if let Some(special) = other.special {
            if self.special.replace(special).is_some() {
                self.errors
                    .push(RegistrationError::new("multiple combine services"));
            }
        }
        self.refuse_without_service_name |= other.refuse_without_service_name;
    }

    fn service(&mut self, name: String) -> &mut ServiceEntry<M> {
        let index = match self.names.get(&name).copied() {
            Some(index) => index,
            None => {
                self.names.insert(name.clone(), self.services.len());
                self.services.push(ServiceEntry {
                    name,
                    methods: FxHashMap::default(),
                });
                self.services.len() - 1
            }
        };
        &mut self.services[index]
    }

    /// Declares an empty service so a known name never falls back by method.
    /// Repeated declarations are idempotent; merging two registrations is checked.
    pub fn declare_service(&mut self, service: impl Into<String>) {
        self.service(service.into());
    }

    pub fn add_method(
        &mut self,
        service: impl Into<String>,
        method: impl Into<String>,
        record: M,
    ) -> Result<(), RegistrationError> {
        let entry = self.service(service.into());
        let method = method.into();
        if entry.methods.contains_key(&method) {
            return Err(RegistrationError::new(format!(
                "duplicate method registration: {}/{method}",
                entry.name
            )));
        }
        entry.methods.insert(method, record);
        Ok(())
    }

    pub fn combine_service(&mut self, service: impl Into<String>) -> Result<(), RegistrationError> {
        if self.special.is_some() {
            return Err(RegistrationError::new("multiple combine services"));
        }
        self.special = Some(service.into());
        Ok(())
    }

    pub fn refuse_traffic_without_service_name(&mut self, refuse: bool) {
        self.refuse_without_service_name = refuse;
    }

    pub fn fallback(&mut self, service: impl Into<String>) {
        if self.fallback.is_some() {
            self.errors
                .push(RegistrationError::new("multiple fallback services"));
        }
        self.fallback = Some(service.into());
    }

    pub fn validate(&self) -> Result<(), RegistrationError> {
        if let Some(error) = self.errors.first() {
            return Err(error.clone());
        }
        if self.services.is_empty() {
            return Err(RegistrationError::new("no services registered"));
        }
        if let Some(name) = &self.fallback {
            if !self.services.iter().any(|service| &service.name == name) {
                return Err(RegistrationError::new("fallback service is not registered"));
            }
        }
        if let Some(name) = &self.special {
            if self.services.len() != 1 || &self.services[0].name != name {
                return Err(RegistrationError::new(
                    "combine service must be the only service",
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
                    return Err(RegistrationError::new(format!(
                        "method {method} conflicts across services without a fallback"
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn find<'a>(&'a self, service: &str, method: &str) -> Option<(&'a str, M)>
    where
        M: Clone,
    {
        self.lookup(service, method, self.refuse_without_service_name)
    }

    /// Strictness is local to this query and never mutates Thrift lookup policy.
    pub fn find_strict<'a>(&'a self, service: &str, method: &str) -> Option<(&'a str, M)>
    where
        M: Clone,
    {
        self.lookup(service, method, true)
    }

    fn named(&self, name: &str) -> Option<&ServiceEntry<M>> {
        if self.services.len() <= 4 {
            self.services.iter().find(|entry| entry.name == name)
        } else {
            self.names.get(name).map(|&index| &self.services[index])
        }
    }

    fn lookup<'a>(&'a self, service: &str, method: &str, strict: bool) -> Option<(&'a str, M)>
    where
        M: Clone,
    {
        let selected = if strict {
            self.named(service)
        } else if let Some(name) = &self.special {
            self.named(name)
        } else if service.is_empty() {
            self.services
                .iter()
                .find(|entry| {
                    Some(&entry.name) == self.fallback.as_ref()
                        && entry.methods.contains_key(method)
                })
                .or_else(|| {
                    self.services.iter().find(|entry| {
                        Some(&entry.name) != self.fallback.as_ref()
                            && entry.methods.contains_key(method)
                    })
                })
        } else if let Some(exact) = self.named(service) {
            Some(exact)
        } else {
            let mut matches = self
                .services
                .iter()
                .filter(|entry| entry.methods.contains_key(method));
            let first = matches.next();
            if matches.next().is_none() {
                first
            } else {
                None
            }
        };
        let selected = selected?;
        selected
            .methods
            .get(method)
            .cloned()
            .map(|record| (selected.name.as_str(), record))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn special_services_only_bypass_names_in_non_strict_queries() {
        let mut directory = ServiceDirectory::new();
        directory.add_method("Combined", "call", 1).unwrap();
        directory.combine_service("Combined").unwrap();
        directory.validate().unwrap();
        assert_eq!(directory.find("wrong", "call"), Some(("Combined", 1)));
        assert_eq!(directory.find_strict("wrong", "call"), None);
        assert_eq!(directory.find_strict("", "call"), None);
        assert_eq!(
            directory.find_strict("Combined", "call"),
            Some(("Combined", 1))
        );
        directory.refuse_traffic_without_service_name(true);
        assert_eq!(directory.find("wrong", "call"), None);
        assert_eq!(directory.find("", "call"), None);
    }

    #[test]
    fn strict_queries_share_records_without_changing_non_strict_lookup() {
        let mut directory = ServiceDirectory::new();
        directory.add_method("A", "shared", "A/shared").unwrap();
        directory.add_method("A", "only_a", "A/only_a").unwrap();
        directory.add_method("B", "shared", "B/shared").unwrap();
        directory.add_method("B", "only_b", "B/only_b").unwrap();
        assert!(directory.validate().is_err());
        directory.fallback("B");
        directory.validate().unwrap();

        let directory = Arc::new(directory);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let directory = directory.clone();
                scope.spawn(move || {
                    for _ in 0..100 {
                        assert_eq!(directory.find_strict("wrong", "only_a"), None);
                        assert_eq!(directory.find_strict("", "shared"), None);
                        assert_eq!(directory.find_strict("A", "only_b"), None);
                        assert_eq!(
                            directory.find_strict("A", "shared"),
                            Some(("A", "A/shared"))
                        );
                        assert_eq!(directory.find("", "shared"), Some(("B", "B/shared")));
                        assert_eq!(directory.find("wrong", "shared"), None);
                        assert_eq!(directory.find("wrong", "only_a"), Some(("A", "A/only_a")));
                    }
                });
            }
        });
    }

    #[test]
    fn known_method_with_no_transport_binding_still_wins_selection() {
        let mut directory = ServiceDirectory::new();
        directory.add_method("Unary", "shared", None).unwrap();
        directory.add_method("Stream", "shared", Some(1)).unwrap();
        directory.fallback("Unary");
        directory.validate().unwrap();
        assert_eq!(directory.find("", "shared"), Some(("Unary", None)));
        assert_eq!(
            directory.find_strict("Unary", "shared"),
            Some(("Unary", None))
        );
        assert_eq!(directory.find("wrong", "shared"), None);
        assert_eq!(
            directory.find("Stream", "shared"),
            Some(("Stream", Some(1)))
        );
    }

    #[test]
    fn combine_merge_and_empty_service_keep_original_rules() {
        let mut directory = ServiceDirectory::new();
        directory.declare_service("Empty");
        directory.add_method("Typed", "call", 1).unwrap();
        directory.validate().unwrap();
        assert_eq!(directory.find("Empty", "call"), None);
        assert_eq!(directory.find("wrong", "anything"), None);

        let mut duplicate = ServiceDirectory::new();
        duplicate.add_method("Typed", "other", 4).unwrap();
        directory.merge(duplicate);
        assert!(
            directory
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate service")
        );

        let mut combined = ServiceDirectory::new();
        combined.add_method("Combined", "call", 5).unwrap();
        combined.combine_service("Combined").unwrap();
        combined.validate().unwrap();
        assert_eq!(combined.find("wrong", "call"), Some(("Combined", 5)));
        assert_eq!(combined.find_strict("wrong", "call"), None);
        combined.add_method("Other", "other", 6).unwrap();
        assert!(combined.validate().is_err());
    }
}
