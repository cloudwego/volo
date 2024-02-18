use motore::{layer::Layer, service::Service};
use volo::context::Context;

#[cfg(feature = "server")]
mod server;

#[derive(Debug, Default, Clone, Copy)]
pub struct Extension<T>(pub T);

impl<S, T> Layer<S> for Extension<T>
where
    S: Send + Sync + 'static,
    T: Sync,
{
    type Service = ExtensionService<S, T>;

    fn layer(self, inner: S) -> Self::Service {
        ExtensionService { inner, ext: self.0 }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ExtensionService<I, T> {
    inner: I,
    ext: T,
}

impl<S, Cx, Req, Resp, E, T> Service<Cx, Req> for ExtensionService<S, T>
where
    S: Service<Cx, Req, Response = Resp, Error = E> + Send + Sync + 'static,
    Req: Send,
    Cx: Context + Send,
    T: Clone + Send + Sync + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut Cx,
        req: Req,
    ) -> Result<Self::Response, Self::Error> {
        cx.extensions_mut().insert(self.ext.clone());
        self.inner.call(cx, req).await
    }
}
