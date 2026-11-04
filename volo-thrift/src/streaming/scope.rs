//! One boundary around a server handler future, independent of message codecs.
use std::{future::Future, pin::Pin};

use super::{StreamContext, StreamError};

pub type StreamHandlerFuture =
    Pin<Box<dyn Future<Output = Result<(), StreamError>> + Send + 'static>>;

/// Installs application task-local scopes or maps the final handler result.
///
/// Invoked once per stream inside `StreamContext::scope`. A spawned child task
/// must explicitly carry its own context and task-local scopes.
pub trait StreamHandlerScope: Send + Sync + 'static {
    fn scope(&self, cx: StreamContext, future: StreamHandlerFuture) -> StreamHandlerFuture;
}
