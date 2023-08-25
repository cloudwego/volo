use std::{future::Future, marker::PhantomData};

use http::Response;
use hyper::body::Incoming;

use crate::{request::FromRequest, response::RespBody, DynError, HttpContext};

pub(crate) struct DispatchService<S, IB, OB> {
    inner: S,
    _marker: PhantomData<(IB, OB)>,
}

impl<S, IB, OB> DispatchService<S, IB, OB> {
    pub(crate) fn new(service: S) -> Self {
        Self {
            inner: service,
            _marker: PhantomData,
        }
    }
}

impl<S, IB, OB> Clone for DispatchService<S, IB, OB>
where
    S: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

unsafe impl<S, IB, OB> Send for DispatchService<S, IB, OB> where S: Send {}

unsafe impl<S, IB, OB> Sync for DispatchService<S, IB, OB> where S: Sync {}

impl<S, IB, OB> motore::Service<HttpContext, Incoming> for DispatchService<S, IB, OB>
where
    S: motore::Service<HttpContext, IB, Response = Response<OB>> + Send + Sync + 'static,
    S::Error: std::error::Error + Send + Sync + 'static,
    OB: Into<RespBody>,
    IB: FromRequest + Send,
    for<'cx> <IB as FromRequest>::FromFut<'cx>: std::marker::Send,
{
    type Response = Response<RespBody>;

    type Error = DynError;

    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + Send + 'cx
    where
        HttpContext: 'cx,
        Self: 'cx;

    fn call<'cx, 's>(&'s self, cx: &'cx mut HttpContext, req: Incoming) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        async move {
            match IB::from(&*cx, req).await {
                Ok(body) => self
                    .inner
                    .call(cx, body)
                    .await
                    .map(|resp| {
                        let (parts, body) = resp.into_parts();
                        Response::from_parts(parts, body.into())
                    })
                    .map_err(|e| Box::new(e) as DynError),
                Err(response) => Ok(response),
            }
        }
    }
}
