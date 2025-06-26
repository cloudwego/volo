//! Utilities for using [`tracing::span!`] for [`Server`]
//!
//! [`Server`]: crate::server::Server

use tracing::Span;

use crate::context::ServerContext;

/// Provider for [`Span`].
///
/// [`SpanProvider`] will be used as a hook by [`Server`]. When starting to serve a request, the
/// serve function will call [`SpanProvider::on_serve`] and enter the [`Span`] it returns. When
/// leaving the serve function scope, [`SpanProvider::leave_serve`] will be called to perform some
/// operations of [`Span`].
///
/// [`Span`]: tracing::Span
/// [`Server`]: crate::server::Server
pub trait SpanProvider {
    /// Handler that will be called when starting to serve a request.
    ///
    /// It should return a [`Span`] and the serve function will enter it.
    fn on_serve(&self, context: &ServerContext) -> Span {
        let _ = context;
        Span::none()
    }

    /// Handler that will be called when leaving the serve function.
    fn leave_serve(&self, context: &ServerContext) {
        let _ = context;
    }
}

/// Default implementation of [`SpanProvider`] that do nothing.
#[derive(Debug, Default, Clone)]
pub struct DefaultProvider;

impl SpanProvider for DefaultProvider {}
