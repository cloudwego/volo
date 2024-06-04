//! A layer that catches panics and calls a handler.
//!
//! The `Handler` is called with the context and the panic value,
//! and should return a response to be returned to the client, or an error to be
//! propagated.
//!
//! For example of `Handler` implementations, see the `server::panic_handler` module in
//! `volo-thrift` crate.
//!
//! For example of usage, see the `examples/src/hello/thrift_server_panic.rs` file in the repo.

use std::{fmt, panic::AssertUnwindSafe};

use faststr::FastStr;
use futures::FutureExt;

/// A layer that catches panics and calls a handler.
///
/// Users are supposed to use this layer directly, and put it in the front.
///
/// # Example
///
/// ```rust,no_run
/// server.layer_front(volo::catch_panic::Layer::new(
///     volo_thrift::server::panic_handler::log_and_return_exception,
/// ))
/// ```
///
/// Note: not all panics can be caught, for example, if users call `std::process::exit`,
/// `std::process::abort` or set panic = "abort" in profile, the process will exit immediately.
pub struct Layer<T> {
    panic_handler: T,
}

impl<T> Layer<T> {
    /// Create a new `Layer` with the given `panic_handler`.
    pub fn new(panic_handler: T) -> Self {
        Self { panic_handler }
    }
}

/// Contains information about a panic.
///
/// Since `std::panic::PanicInfo` can only be obtained in the panic hook, we provide this struct
/// to store the panic information.
#[derive(Debug)]
pub struct PanicInfo {
    /// The panic message formatted by `std::panic::PanicInfo`.
    ///
    /// This is provided for convenience, because we cannot get `std::panic::PanicInfo` in
    /// `catch_unwind`.
    pub message: FastStr,
    /// The location where the panic occurred.
    ///
    /// This is also taken out from `std::panic::PanicInfo`.
    pub location: Option<Location>,
    /// The backtrace of the panic.
    ///
    /// This is captured by `std::backtrace::Backtrace::capture` in the panic hook.
    pub backtrace: std::backtrace::Backtrace,
}

impl fmt::Display for PanicInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("panicked at ")?;
        if let Some(location) = &self.location {
            location.fmt(formatter)?;
            formatter.write_str(":")?;
        }
        formatter.write_str("\nmessage: ")?;
        self.message.fmt(formatter)?;
        formatter.write_str("\n backtrace: \n")?;
        self.backtrace.fmt(formatter)?;
        Ok(())
    }
}

/// The `std::panic::Location` has a lifetime so we cannot store it directly, thus we
/// make a copy here.
#[derive(Debug, Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Location {
    pub file: FastStr,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Location {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}:{}", self.file, self.line, self.col)
    }
}

thread_local! {
    static PANIC_INFO: std::cell::RefCell<Option<PanicInfo>> = std::cell::RefCell::new(None);
}
static PANIC_HOOK_INIT: std::sync::Once = std::sync::Once::new();

/// Initialize the panic hook, which will capture the panic information and store it in a thread
/// local variable. This information can be used by the `Handler` to handle the panic.
///
/// This function should only be called only once, although there's once guard inside.
///
/// The `Layer` will call this function to initialize the panic hook, so normally you don't need to
/// call it directly.
pub fn init_panic_hook() {
    PANIC_HOOK_INIT.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let backtrace = std::backtrace::Backtrace::capture();
            let message = panic_info.to_string().into();
            let location = panic_info.location().map(|l| Location {
                file: FastStr::new(l.file()),
                line: l.line(),
                col: l.column(),
            });
            PANIC_INFO.with(|info| {
                *info.borrow_mut() = Some(PanicInfo {
                    message,
                    location,
                    backtrace,
                });
            });
            // still call the previous hook
            default_hook(panic_info);
        }));
    });
}

pub trait Handler<S, Cx, Req>
where
    S: crate::Service<Cx, Req> + Send + Sync + 'static,
    Cx: Send + 'static,
    Req: Send + 'static,
{
    fn handle(
        &self,
        cx: &mut Cx,
        payload: Box<dyn std::any::Any + Send>,
        panic_info: PanicInfo,
    ) -> Result<S::Response, S::Error>;
}

/// Impl this Handler for F so users can use a closure as the panic handler.
impl<F, S, Cx, Req> Handler<S, Cx, Req> for F
where
    F: Fn(&mut Cx, Box<dyn std::any::Any + Send>, PanicInfo) -> Result<S::Response, S::Error>,
    S: crate::Service<Cx, Req> + Send + Sync + 'static,
    Cx: Send + 'static,
    Req: Send + 'static,
{
    // `Handler` should be called rarely, so don't inline it here to reduce the code size and
    // improve the performance of the happy path.
    #[inline(never)]
    fn handle(
        &self,
        cx: &mut Cx,
        payload: Box<dyn std::any::Any + Send>,
        panic_info: PanicInfo,
    ) -> Result<S::Response, S::Error> {
        self(cx, payload, panic_info)
    }
}

impl<S, T> crate::layer::Layer<S> for Layer<T> {
    type Service = Service<S, T>;

    #[inline]
    fn layer(self, inner: S) -> Self::Service {
        init_panic_hook();
        Service {
            inner,
            panic_handler: self.panic_handler,
        }
    }
}

pub struct Service<S, T> {
    inner: S,
    panic_handler: T,
}

impl<Cx, Req, S, T> crate::Service<Cx, Req> for Service<S, T>
where
    S: crate::Service<Cx, Req> + Send + Sync + 'static,
    T: Handler<S, Cx, Req> + Send + Sync,
    Cx: Send + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    #[inline]
    async fn call(&self, cx: &mut Cx, req: Req) -> Result<Self::Response, Self::Error> {
        // `self.inner.call` is used to create the inner future, which is also possible to panic
        let payload = match std::panic::catch_unwind(AssertUnwindSafe(|| self.inner.call(cx, req)))
        {
            Ok(future) => match AssertUnwindSafe(future).catch_unwind().await {
                Ok(resp) => return resp,
                Err(err) => err,
            },
            Err(err) => err,
        };
        let panic_info = PANIC_INFO
            .with(|info| info.borrow_mut().take())
            .expect("[Volo] panic_info missing when handling panic");
        self.panic_handler.handle(cx, payload, panic_info)
    }
}
