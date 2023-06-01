use tracing::Span;

use crate::context::ServerContext;

pub trait SpanProvider: 'static + Send + Sync + Clone {
    fn on_serve(&self) -> Span {
        Span::none()
    }

    fn on_decode(&self) -> Span {
        Span::none()
    }

    fn on_encode(&self) -> Span {
        Span::none()
    }

    fn leave_decode(&self, context: &ServerContext) {
        let _ = context;
    }

    fn leave_encode(&self, context: &ServerContext) {
        let _ = context;
    }

    fn leave_serve(&self, context: &ServerContext) {
        let _ = context;
    }
}

#[derive(Clone)]
pub struct DefaultProvider;

impl SpanProvider for DefaultProvider {}
