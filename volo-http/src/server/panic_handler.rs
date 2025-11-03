//! Collections for some panic handlers
//!
//! [`volo::catch_panic::Layer`] can handle panics in services and when panic occurs, it can
//! respond a [`Response`]. This module has some useful handlers for handling panics and
//! returning a response.

use std::any::Any;

use http::StatusCode;
use motore::service::Service;
use volo::catch_panic;

use super::IntoResponse;
use crate::response::Response;

/// Panic handler which can return a fixed payload.
///
/// This type can be constructed by [`fixed_payload`].
#[derive(Clone, Debug)]
pub struct FixedPayload<R> {
    payload: R,
}

impl<S, Cx, Req, Resp> catch_panic::Handler<S, Cx, Req> for FixedPayload<Resp>
where
    S: Service<Cx, Req, Response = Response> + Send + Sync + 'static,
    Cx: Send + 'static,
    Req: Send + 'static,
    Resp: IntoResponse + Clone,
{
    fn handle(
        &self,
        _: &mut Cx,
        _: Box<dyn Any + Send>,
        panic_info: catch_panic::PanicInfo,
    ) -> Result<S::Response, S::Error> {
        tracing::error!("[Volo-HTTP] panic_handler: {panic_info}");
        Ok(self.payload.clone().into_response())
    }
}

/// This function is a panic handler and can work with [`volo::catch_panic::Layer`], it will always
/// return `500 Internal Server Error`.
pub fn always_internal_error<Cx, E>(
    _: &mut Cx,
    _: Box<dyn Any + Send>,
    panic_info: catch_panic::PanicInfo,
) -> Result<Response, E> {
    tracing::error!("[Volo-HTTP] panic_handler: {panic_info}");
    Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Create a panic handler which can work with [`volo::catch_panic::Layer`]. The handler will
/// always return the specified fixed payload as response.
pub fn fixed_payload<R>(payload: R) -> FixedPayload<R> {
    FixedPayload { payload }
}

#[cfg(test)]
mod panic_handler_tests {
    use std::{convert::Infallible, marker::PhantomData};

    use http::status::StatusCode;
    use motore::{layer::Layer, service::Service};

    use super::{always_internal_error, fixed_payload};
    use crate::{
        body::{Body, BodyConversion},
        request::Request,
        response::Response,
        server::{IntoResponse, test_helpers},
    };

    struct PanicService<R = Response, E = Infallible> {
        _marker: PhantomData<fn(R, E)>,
    }

    impl<R, E> PanicService<R, E> {
        const fn new() -> Self {
            Self {
                _marker: PhantomData,
            }
        }
    }

    impl<Cx, Req, Resp, E> Service<Cx, Req> for PanicService<Resp, E>
    where
        Cx: Send,
        Req: Send,
        Resp: Send,
    {
        type Response = Resp;
        type Error = E;

        async fn call(&self, _: &mut Cx, _: Req) -> Result<Self::Response, Self::Error> {
            panic!("oops")
        }
    }

    #[tokio::test]
    async fn fixed_payload_test() {
        const ERR_MSG: &str = "oops";

        // it's ok
        {
            let srv =
                volo::catch_panic::Layer::new(fixed_payload((StatusCode::NOT_FOUND, ERR_MSG)))
                    .layer(test_helpers::DefaultService::<Response, Infallible>::new());
            let resp = srv
                .call(&mut test_helpers::empty_cx(), Request::<Body>::default())
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        // it panics
        {
            let srv =
                volo::catch_panic::Layer::new(fixed_payload((StatusCode::NOT_FOUND, ERR_MSG)))
                    .layer(PanicService::<Response, Infallible>::new());
            let resp = srv
                .call(&mut test_helpers::empty_cx(), Request::<Body>::default())
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            assert_eq!(resp.into_string().await.unwrap(), ERR_MSG);
        }
    }

    #[tokio::test]
    async fn internal_error_test() {
        // it's ok
        {
            let srv = volo::catch_panic::Layer::new(always_internal_error)
                .layer(test_helpers::DefaultService::<Response, Infallible>::new());
            let resp = srv
                .call(&mut test_helpers::empty_cx(), Request::<Body>::default())
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        // it panics
        {
            let srv = volo::catch_panic::Layer::new(always_internal_error).layer(PanicService::<
                Response,
                Infallible,
            >::new(
            ));
            let resp = srv
                .call(&mut test_helpers::empty_cx(), Request::<Body>::default())
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
}
