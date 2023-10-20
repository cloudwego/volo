use futures::Future;
use motore::Service;

pub trait ClientService<Cx, Req>: Service<Cx, Req> {}

pub struct WithOptService<S, Opt> {
    inner: S,
    opt: Opt,
}

impl<S, Opt> WithOptService<S, Opt> {
    pub fn new(inner: S, opt: Opt) -> Self {
        Self { inner, opt }
    }
}

pub trait Apply<Cx> {
    type Error;

    fn apply(self, cx: &mut Cx) -> Result<(), Self::Error>;
}

pub trait OneShotService<Cx, Request> {
    /// Responses given by the service.
    type Response;
    /// Errors produced by the service.
    type Error;

    /// Process the request and return the response asynchronously.
    fn call<'cx>(
        self,
        cx: &'cx mut Cx,
        req: Request,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send;
}

impl<S, Cx, Req, Opt> OneShotService<Cx, Req> for WithOptService<S, Opt>
where
    Cx: 'static + Send,
    Opt: 'static + Send + Sync + Apply<Cx, Error = S::Error>,
    Req: 'static + Send,
    S: Service<Cx, Req> + 'static + Sync + Send,
{
    type Response = S::Response;

    type Error = S::Error;

    #[inline]
    fn call<'cx>(
        self,
        cx: &'cx mut Cx,
        req: Req,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        async move {
            self.opt.apply(cx)?;
            self.inner.call(cx, req).await
        }
    }
}

pub trait MkClient<S> {
    type Target;
    fn mk_client(&self, service: S) -> Self::Target;
}
