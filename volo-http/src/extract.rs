use futures_util::Future;
use http::{Method, Response, Uri};
use volo::net::Address;

use crate::{response::IntoResponse, HttpContext, Params, State};

pub trait FromContext<S>: Sized {
    type Rejection: IntoResponse;
    fn from_context(
        context: &HttpContext,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

impl<T, S> FromContext<S> for Option<T>
where
    T: FromContext<S>,
    S: Send + Sync,
{
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &HttpContext, state: &S) -> Result<Self, Self::Rejection> {
        Ok(T::from_context(context, state).await.ok())
    }
}

impl<S> FromContext<S> for Address
where
    S: Send + Sync,
{
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Address, Self::Rejection> {
        Ok(context.peer.clone())
    }
}

impl<S> FromContext<S> for Uri
where
    S: Send + Sync,
{
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Uri, Self::Rejection> {
        Ok(context.uri.clone())
    }
}

impl<S> FromContext<S> for Method
where
    S: Send + Sync,
{
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Method, Self::Rejection> {
        Ok(context.method.clone())
    }
}

impl<S> FromContext<S> for Params
where
    S: Send + Sync,
{
    type Rejection = Response<()>; // Infallible

    async fn from_context(context: &HttpContext, _state: &S) -> Result<Params, Self::Rejection> {
        Ok(context.params.clone())
    }
}

impl<S> FromContext<S> for State<S>
where
    S: Clone + Send + Sync,
{
    type Rejection = Response<()>; // Infallible

    async fn from_context(_context: &HttpContext, state: &S) -> Result<Self, Self::Rejection> {
        Ok(State(state.clone()))
    }
}
