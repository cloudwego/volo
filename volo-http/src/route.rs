use std::collections::HashMap;

use http::{Method, Response, StatusCode};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use motore::{layer::Layer, Service};

use crate::{
    dispatch::DispatchService, request::FromRequest, response::RespBody, DynError, HttpContext,
};

// The `matchit::Router` cannot be converted to `Iterator`, so using `matchit::Router<DynService>`
// is not convenient enough.
//
// To solve the problem, we refer to the implementation of `axum` and introduce a `RouteId` as a
// bridge, the `matchit::Router` only handles some IDs and each ID corresponds to a `DynService`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RouteId(u32);

impl RouteId {
    fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        // `AtomicU64` isn't supported on all platforms
        static ID: AtomicU32 = AtomicU32::new(0);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        if id == u32::MAX {
            panic!("Over `u32::MAX` routes created. If you need this, please file an issue.");
        }
        Self(id)
    }
}

pub type DynService = motore::BoxCloneService<HttpContext, Incoming, Response<RespBody>, DynError>;

#[derive(Clone, Default)]
pub struct Router {
    matcher: matchit::Router<RouteId>,
    routes: HashMap<RouteId, DynService>,
}

impl Service<HttpContext, Incoming> for Router {
    type Response = Response<RespBody>;

    type Error = DynError;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        if let Ok(matched) = self.matcher.at(cx.uri.path()) {
            if let Some(srv) = self.routes.get(matched.value) {
                cx.params = matched.params.into();
                return srv.call(cx, req).await;
            }
        }

        Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::new()).into())
            .unwrap())
    }
}

impl Router {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn route<R, S>(mut self, uri: R, route: S) -> Self
    where
        R: Into<String>,
        S: Service<HttpContext, Incoming, Response = Response<RespBody>, Error = DynError>
            + Send
            + Sync
            + Clone
            + 'static,
    {
        let route_id = RouteId::next();
        if let Err(e) = self.matcher.insert(uri, route_id) {
            panic!("Insert routing rule failed, error: {e}");
        }
        self.routes
            .insert(route_id, motore::BoxCloneService::new(route));

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

impl Service<HttpContext, Incoming> for Route {
    type Response = Response<RespBody>;

    type Error = DynError;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
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

macro_rules! impl_method_register {
    ($( $method:ident ),*) => {
        $(
        pub fn $method<S, IB, OB>(mut self, handler: S) -> Self
        where
            S: Service<HttpContext, IB, Response = Response<OB>>
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
