//! Call options for requests
//!
//! See [`CallOpt`] for more details.

use std::time::Duration;

use faststr::FastStr;
use metainfo::{FastStrMap, TypeMap};
use volo::{client::Apply, context::Context};

use crate::{context::ClientContext, error::ClientError};

/// Call options for requests
#[derive(Debug, Default)]
pub struct CallOpt {
    /// Timeout of the whole request
    ///
    /// This timeout includes connect, sending request headers, receiving response headers, but
    /// without receiving streaming data.
    pub timeout: Option<Duration>,
    /// Additional information of the endpoint.
    ///
    /// Users can use `tags` to store custom data, such as the datacenter name or the region name,
    /// which can be used by the service discoverer.
    pub tags: TypeMap,
    /// A optimized typemap for storing additional information of the endpoint.
    ///
    /// Use [`FastStrMap`] instead of [`TypeMap`] can reduce the Box allocation.
    ///
    /// This is mainly for performance optimization.
    pub faststr_tags: FastStrMap,
}

impl CallOpt {
    /// Create a new [`CallOpt`].
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a timeout for the [`CallOpt`].
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = Some(timeout);
    }

    /// Consume current [`CallOpt`] and return a new one with the given timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Check if [`CallOpt`] tags contain entry.
    #[inline]
    pub fn contains<T: 'static>(&self) -> bool {
        self.tags.contains::<T>()
    }

    /// Insert a tag into this [`CallOpt`].
    #[inline]
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.tags.insert(val);
    }

    /// Consume current [`CallOpt`] and return a new one with the tag.
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

    /// Check if [`CallOpt`] tags contain entry.
    #[inline]
    pub fn contains_faststr<T: 'static>(&self) -> bool {
        self.faststr_tags.contains::<T>()
    }

    /// Insert a tag into this [`CallOpt`].
    #[inline]
    pub fn insert_faststr<T: Send + Sync + 'static>(&mut self, val: FastStr) {
        self.faststr_tags.insert::<T>(val);
    }

    /// Consume current [`CallOpt`] and return a new one with the tag.
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

impl Apply<ClientContext> for CallOpt {
    type Error = ClientError;

    fn apply(self, cx: &mut ClientContext) -> Result<(), Self::Error> {
        {
            let callee = cx.rpc_info_mut().callee_mut();
            if !self.tags.is_empty() {
                callee.tags.extend(self.tags);
            }
            if !self.faststr_tags.is_empty() {
                callee.faststr_tags.extend(self.faststr_tags);
            }
        }
        {
            let config = cx.rpc_info_mut().config_mut();
            if self.timeout.is_some() {
                config.set_timeout(self.timeout);
            }
        }
        Ok(())
    }
}
