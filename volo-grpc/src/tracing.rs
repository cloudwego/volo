use tracing::Span;

use crate::context::ServerContext;

pub trait SpanProvider: 'static + Send + Sync + Clone {
    fn on_serve(&self, context: &ServerContext) -> Span {
        let _ = context;
        Span::none()
    }

    fn on_encode(&self, context: &ServerContext) -> Span {
        let _ = context;
        Span::none()
    }

    fn leave_encode(&self, context: &ServerContext) {
        let _ = context;
    }

    fn leave_serve(&self, context: &ServerContext) {
        let _ = context;
    }
}

#[derive(Clone, Copy)]
pub struct DefaultProvider;

impl SpanProvider for DefaultProvider {}
