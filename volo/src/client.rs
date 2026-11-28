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

    /// Explicit per-call RPC timeout, without applying a protocol's implicit default.
    /// Consumers with a distinct call lifetime can use it before consuming the option.
    fn rpc_timeout(&self) -> Option<std::time::Duration> {
        None
    }

    fn apply(self, cx: &mut Cx) -> Result<(), Self::Error>;
}

// Streaming opens consume the same per-call option while using their own
// transport stack; the unary request wrapper remains source compatible.
impl<S, Cx, Opt: Apply<Cx>> Apply<Cx> for WithOptService<S, Opt> {
    type Error = Opt::Error;
    fn rpc_timeout(&self) -> Option<std::time::Duration> {
        <Opt as Apply<Cx>>::rpc_timeout(&self.opt)
    }
    fn apply(self, cx: &mut Cx) -> Result<(), Self::Error> {
        self.opt.apply(cx)
    }
}

pub trait OneShotService<Cx, Request> {
    /// Responses given by the service.
    type Response;
    /// Errors produced by the service.
    type Error;

    /// Process the request and return the response asynchronously.
    fn call(
        self,
        cx: &mut Cx,
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
    async fn call(self, cx: &mut Cx, req: Req) -> Result<Self::Response, Self::Error> {
        self.opt.apply(cx)?;
        self.inner.call(cx, req).await
    }
}

pub trait MkClient<S> {
    type Target;
    fn mk_client(&self, service: S) -> Self::Target;
}
