use std::future::Future;

use http::{Method, Response, StatusCode};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use motore::layer::Layer;

use crate::{
    dispatch::DispatchService, request::FromRequest, response::RespBody, DynError, HttpContext,
};

pub type DynService = motore::BoxCloneService<HttpContext, Incoming, Response<RespBody>, DynError>;

#[derive(Clone, Default)]
pub struct Router {
    inner: matchit::Router<DynService>,
}

impl motore::Service<HttpContext, Incoming> for Router {
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
            if let Ok(matched) = self.inner.at(cx.uri.path()) {
                cx.params = matched.params.into();
                matched.value.call(cx, req).await
            } else {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::new()).into())
                    .unwrap())
            }
        }
    }
}

impl Router {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn route<R, S>(mut self, uri: R, route: S) -> Self
    where
        R: Into<String>,
        S: motore::Service<HttpContext, Incoming, Response = Response<RespBody>, Error = DynError>
            + Send
            + Sync
            + Clone
            + 'static,
    {
        if let Err(e) = self.inner.insert(uri, motore::BoxCloneService::new(route)) {
            panic!("routing error: {e}");
        }
        self
    }
}

pub trait ServiceLayerExt: Sized {
    fn layer<L>(self, l: L) -> L::Service
    where
        L: Layer<Self>;
}

impl<S> ServiceLayerExt for S {
    fn layer<L>(self, l: L) -> L::Service
    where
        L: Layer<Self>,
    {
        Layer::layer(l, self)
    }
}

#[derive(Default, Clone)]
pub struct Route {
    options: Option<DynService>,
    get: Option<DynService>,
    post: Option<DynService>,
    put: Option<DynService>,
    delete: Option<DynService>,
    head: Option<DynService>,
    trace: Option<DynService>,
    connect: Option<DynService>,
    patch: Option<DynService>,
}

impl Route {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn builder() -> RouteBuilder {
        RouteBuilder { route: Self::new() }
    }
}

impl motore::Service<HttpContext, Incoming> for Route {
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
            match cx.method {
                Method::GET => {
                    if let Some(service) = &self.get {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::POST => {
                    if let Some(service) = &self.post {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::PUT => {
                    if let Some(service) = &self.put {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::DELETE => {
                    if let Some(service) = &self.delete {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::HEAD => {
                    if let Some(service) = &self.head {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::OPTIONS => {
                    if let Some(service) = &self.options {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::CONNECT => {
                    if let Some(service) = &self.connect {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::PATCH => {
                    if let Some(service) = &self.patch {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                Method::TRACE => {
                    if let Some(service) = &self.trace {
                        service.call(cx, req).await
                    } else {
                        Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body("".into())
                            .unwrap())
                    }
                }
                _ => Ok(Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .body("".into())
                    .unwrap()),
            }
        }
    }
}

macro_rules! impl_method_register {
    ($( $method:ident ),*) => {
        $(
        pub fn $method<S, IB, OB>(mut self, handler: S) -> Self
        where
            S: motore::Service<HttpContext, IB, Response = Response<OB>>
                + Send
                + Sync
                + Clone
                + 'static,
            S::Error: std::error::Error + Send + Sync,
            OB: Into<RespBody> + 'static,
            IB: FromRequest + Send + 'static,
        {
            self.route.$method = Some(motore::BoxCloneService::new(DispatchService::new(handler)));
            self
        }
        )+
    };
}

pub struct RouteBuilder {
    route: Route,
}

impl RouteBuilder {
    impl_method_register!(options, get, post, put, delete, head, trace, connect, patch);

    pub fn build(self) -> Route {
        self.route
    }
}
