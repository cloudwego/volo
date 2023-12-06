use futures_util::Future;
use hyper::http::{Method, Uri};
use volo::net::Address;

use crate::{response::Infallible, HttpContext, Params, State};

pub trait FromContext<S>: Sized {
    fn from_context(
        context: &HttpContext,
        state: &S,
    ) -> impl Future<Output = Result<Self, Infallible>> + Send;
}

impl<T, S> FromContext<S> for Option<T>
where
    T: FromContext<S>,
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, state: &S) -> Result<Self, Infallible> {
        Ok(T::from_context(context, state).await.ok())
    }
}

impl<S> FromContext<S> for Address
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Address, Infallible> {
        Ok(context.peer.clone())
    }
}

impl<S> FromContext<S> for Uri
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Uri, Infallible> {
        Ok(context.uri.clone())
    }
}

impl<S> FromContext<S> for Method
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Method, Infallible> {
        Ok(context.method.clone())
    }
}

impl<S> FromContext<S> for Params
where
    S: Send + Sync,
{
    async fn from_context(context: &HttpContext, _state: &S) -> Result<Params, Infallible> {
        Ok(context.params.clone())
    }
}

impl<S> FromContext<S> for State<S>
where
    S: Clone + Send + Sync,
{
    async fn from_context(_context: &HttpContext, state: &S) -> Result<Self, Infallible> {
        Ok(State(state.clone()))
    }
}
