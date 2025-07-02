use pilota::thrift::{ApplicationException, ApplicationExceptionKind};
use volo::catch_panic;

use crate::{ServerError, context::ServerContext};

/// This handler logs the panic info and returns an `InternalServerError` to the client.
#[inline(never)]
pub fn log_and_return_exception<Resp>(
    cx: &mut ServerContext,
    payload: Box<dyn std::any::Any + Send>,
    panic_info: catch_panic::PanicInfo,
) -> Result<Resp, ServerError> {
    let payload_msg = if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        format!("{payload:?}")
    };

    // There may be some redundant information in the panic_info, but it's better to keep it, since
    // it seems that the payload and message are subject to change in the future.
    let message = format!("panicked in biz logic: {payload_msg}, panic_info: {panic_info}");

    tracing::error!("[Volo-Thrift] {}, cx: {:?}", message, cx);
    Err(ServerError::Application(ApplicationException::new(
        ApplicationExceptionKind::INTERNAL_ERROR,
        message,
    )))
}

/// This is a `handler` type that is equivalent to `log_and_return_exception`.
///
/// This type is here only for example.
#[derive(Clone, Copy, Default)]
pub struct LogAndReturnException;

impl<S, Req> catch_panic::Handler<S, ServerContext, Req> for LogAndReturnException
where
    S: volo::service::Service<ServerContext, Req, Error = ServerError> + Send + Sync + 'static,
    Req: Send + 'static,
{
    #[inline(never)]
    fn handle(
        &self,
        cx: &mut ServerContext,
        payload: Box<dyn std::any::Any + Send>,
        panic_info: catch_panic::PanicInfo,
    ) -> Result<S::Response, S::Error> {
        log_and_return_exception(cx, payload, panic_info)
    }
}
