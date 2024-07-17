//! Call options for requests
//!
//! See [`CallOpt`] for more details.

use faststr::FastStr;
use metainfo::{FastStrMap, TypeMap};

/// Call options for requests
///
/// It can be set to a [`Client`][Client] or a [`RequestBuilder`][RequestBuilder]. The
/// [`TargetParser`][TargetParser] will handle [`Target`][Target] and the [`CallOpt`] for
/// applying information to the [`Endpoint`][Endpoint].
///
/// [Client]: crate::client::Client
/// [RequestBuilder]: crate::client::RequestBuilder
/// [TargetParser]: crate::client::target::TargetParser
/// [Target]: crate::client::target::Target
/// [Endpoint]: volo::context::Endpoint
#[derive(Debug, Default)]
pub struct CallOpt {
    /// `tags` is used to store additional information of the endpoint.
    ///
    /// Users can use `tags` to store custom data, such as the datacenter name or the region name,
    /// which can be used by the service discoverer.
    pub tags: TypeMap,
    /// `faststr_tags` is a optimized typemap to store additional information of the endpoint.
    ///
    /// Use [`FastStrMap`] instead of [`TypeMap`] can reduce the Box allocation.
    ///
    /// This is mainly for performance optimization.
    pub faststr_tags: FastStrMap,
}

impl CallOpt {
    /// Create a new [`CallOpt`]
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if [`CallOpt`] tags contain entry
    #[inline]
    pub fn contains<T: 'static>(&self) -> bool {
        self.tags.contains::<T>()
    }

    /// Insert a tag into this [`CallOpt`].
    #[inline]
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.tags.insert(val);
    }

    /// Insert a tag into this [`CallOpt`] and return self.
    #[inline]
    pub fn with<T: Send + Sync + 'static>(mut self, val: T) -> Self {
        self.tags.insert(val);
        self
    }

    /// Get a reference to a tag previously inserted on this [`CallOpt`].
    #[inline]
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.tags.get::<T>()
    }

    /// Check if [`CallOpt`] tags contain entry
    #[inline]
    pub fn contains_faststr<T: 'static>(&self) -> bool {
        self.faststr_tags.contains::<T>()
    }

    /// Insert a tag into this [`CallOpt`].
    #[inline]
    pub fn insert_faststr<T: Send + Sync + 'static>(&mut self, val: FastStr) {
        self.faststr_tags.insert::<T>(val);
    }

    /// Insert a tag into this [`CallOpt`] and return self.
    #[inline]
    pub fn with_faststr<T: Send + Sync + 'static>(mut self, val: FastStr) -> Self {
        self.faststr_tags.insert::<T>(val);
        self
    }

    /// Get a reference to a tag previously inserted on this [`CallOpt`].
    #[inline]
    pub fn get_faststr<T: 'static>(&self) -> Option<&FastStr> {
        self.faststr_tags.get::<T>()
    }
}
