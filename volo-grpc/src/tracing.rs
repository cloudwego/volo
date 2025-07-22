use tracing::Span;

use crate::{context::ServerContext, metadata::MetadataMap};

pub trait SpanProvider: 'static + Send + Sync + Clone {
    fn on_serve(&self, context: &ServerContext, _metadata: &mut MetadataMap) -> Span {
        let _ = context;
        Span::none()
    }

    fn leave_serve(&self, context: &ServerContext) {
        let _ = context;
    }
}

#[derive(Clone, Copy)]
pub struct DefaultProvider;

impl SpanProvider for DefaultProvider {}
