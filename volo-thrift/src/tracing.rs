use tracing::Span;

use crate::context::ServerContext;

pub trait SpanProvider: 'static + Send + Sync + Clone {
    #[inline]
    fn on_serve(&self) -> Span {
        Span::none()
    }

    #[inline]
    fn on_decode(&self) -> Span {
        Span::none()
    }

    #[inline]
    fn on_encode(&self) -> Span {
        Span::none()
    }

    #[inline]
    fn leave_decode(&self, context: &ServerContext) {
        let _ = context;
    }

    #[inline]
    fn leave_encode(&self, context: &ServerContext) {
        let _ = context;
    }

    #[inline]
    fn leave_serve(&self, context: &ServerContext) {
        let _ = context;
    }
}

#[derive(Clone)]
pub struct DefaultProvider;

impl SpanProvider for DefaultProvider {}
