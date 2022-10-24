//! These codes are copied from `tonic/src/response.rs` and may be modified by us.

use std::fmt::Debug;

use http::Extensions;

use crate::metadata::MetadataMap;

#[derive(Debug)]
pub struct Response<T> {
    metadata: MetadataMap,
    message: T,
    extensions: Extensions,
}

impl<T> Response<T> {
    /// Create a new gRPC response.
    pub fn new(message: T) -> Self {
        Self {
            metadata: MetadataMap::new(),
            message,
            extensions: Extensions::new(),
        }
    }

    /// Get a immutable reference to `T`.
    pub fn get_ref(&self) -> &T {
        &self.message
    }

    /// Get a mutable reference to the message
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.message
    }

    /// Get a reference to the custom response metadata.
    pub fn metadata(&self) -> &MetadataMap {
        &self.metadata
    }

    /// Get a mutable reference to the response metadata.
    pub fn metadata_mut(&mut self) -> &mut MetadataMap {
        &mut self.metadata
    }

    /// Consumes `self`, returning the message
    pub fn into_inner(self) -> T {
        self.message
    }

    pub fn into_parts(self) -> (MetadataMap, Extensions, T) {
        (self.metadata, self.extensions, self.message)
    }

    pub fn from_parts(metadata: MetadataMap, extensions: Extensions, message: T) -> Self {
        Self {
            metadata,
            extensions,
            message,
        }
    }

    pub fn from_http(res: http::Response<T>) -> Self {
        let (head, message) = res.into_parts();
        Self {
            metadata: MetadataMap::from_headers(head.headers),
            message,
            extensions: head.extensions,
        }
    }

    pub fn into_http(self) -> http::Response<T> {
        let mut res = http::Response::new(self.message);

        *res.version_mut() = http::Version::HTTP_2;
        *res.headers_mut() = self.metadata.into_headers();
        *res.extensions_mut() = self.extensions;

        res
    }

    /// Returns a reference to the associated extensions.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Returns a mutable reference to the associated extensions.
    pub fn extensions_mut(&mut self) -> &mut Extensions {
        &mut self.extensions
    }

    #[doc(hidden)]
    pub fn map<F, U>(self, f: F) -> Response<U>
    where
        F: FnOnce(T) -> U,
    {
        let message = f(self.message);
        Response {
            metadata: self.metadata,
            message,
            extensions: self.extensions,
        }
    }
}
