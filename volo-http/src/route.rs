use std::collections::HashMap;

use http::{Method, Response, StatusCode};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use motore::{layer::Layer, Service};

use crate::{
    dispatch::DispatchService, request::FromRequest, response::RespBody, DynError, HttpContext,
};

// The `matchit::Router` cannot be converted to `Iterator`, so using
// `matchit::Router<MethodRouter>` is not convenient enough.
//
// To solve the problem, we refer to the implementation of `axum` and introduce a `RouteId` as a
// bridge, the `matchit::Router` only handles some IDs and each ID corresponds to a `MethodRouter`.
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

#[derive(Clone, Default)]
pub struct Router {
    matcher: matchit::Router<RouteId>,
    routes: HashMap<RouteId, MethodRouter>,
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

    pub fn route<R>(mut self, uri: R, route: MethodRouter) -> Self
    where
        R: Into<String>,
    {
        let route_id = RouteId::next();
        if let Err(e) = self.matcher.insert(uri, route_id) {
            panic!("Insert routing rule failed, error: {e}");
        }
        self.routes.insert(route_id, route);

        self
    }

    pub fn layer<L>(self, l: L) -> Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<HttpContext, Incoming, Response = Response<RespBody>, Error = DynError>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let routes = self
            .routes
            .into_iter()
            .map(|(id, route)| {
                let route = route.layer(l.clone());
                (id, route)
            })
            .collect();

        Router {
            matcher: self.matcher,
            routes,
        }
    }
}

#[derive(Default, Clone)]
pub struct MethodRouter {
    options: Option<Route>,
    get: Option<Route>,
    post: Option<Route>,
    put: Option<Route>,
    delete: Option<Route>,
    head: Option<Route>,
    trace: Option<Route>,
    connect: Option<Route>,
    patch: Option<Route>,
}

impl MethodRouter {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn builder() -> MethodRouterBuilder {
        MethodRouterBuilder { route: Self::new() }
    }

    pub fn layer<L>(self, l: L) -> Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<HttpContext, Incoming, Response = Response<RespBody>, Error = DynError>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let Self {
            options,
            get,
            post,
            put,
            delete,
            head,
            trace,
            connect,
            patch,
        } = self;

        let options = options.map(|r| r.layer(l.clone()));
        let get = get.map(|r| r.layer(l.clone()));
        let post = post.map(|r| r.layer(l.clone()));
        let put = put.map(|r| r.layer(l.clone()));
        let delete = delete.map(|r| r.layer(l.clone()));
        let head = head.map(|r| r.layer(l.clone()));
        let trace = trace.map(|r| r.layer(l.clone()));
        let connect = connect.map(|r| r.layer(l.clone()));
        let patch = patch.map(|r| r.layer(l.clone()));

        Self {
            options,
            get,
            post,
            put,
            delete,
            head,
            trace,
            connect,
            patch,
        }
    }
}

impl Service<HttpContext, Incoming> for MethodRouter {
    type Response = Response<RespBody>;

    type Error = DynError;

    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        let handler = match cx.method {
            Method::OPTIONS => &self.options,
            Method::GET => &self.get,
            Method::POST => &self.post,
            Method::PUT => &self.put,
            Method::DELETE => &self.delete,
            Method::HEAD => &self.head,
            Method::TRACE => &self.trace,
            Method::CONNECT => &self.connect,
            Method::PATCH => &self.patch,
            _ => {
                return Ok(Response::builder()
                    .status(StatusCode::METHOD_NOT_ALLOWED)
                    .body("".into())
                    .unwrap());
            }
        };

        if let Some(service) = handler {
            service.call(cx, req).await
        } else {
            Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body("".into())
                .unwrap())
        }
    }
}

pub struct MethodRouterBuilder {
    route: MethodRouter,
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
            self.route.$method = Some(Route::new(DispatchService::new(handler)));
            self
        }
        )+
    };
}

impl MethodRouterBuilder {
    impl_method_register!(options, get, post, put, delete, head, trace, connect, patch);

    pub fn build(self) -> MethodRouter {
        self.route
    }
}

#[derive(Clone)]
pub struct Route(motore::BoxCloneService<HttpContext, Incoming, Response<RespBody>, DynError>);

impl Route {
    pub fn new<S>(inner: S) -> Self
    where
        S: Service<HttpContext, Incoming, Response = Response<RespBody>, Error = DynError>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        Route(motore::BoxCloneService::new(inner))
    }

    pub fn layer<L>(self, l: L) -> Self
    where
        L: Layer<Route>,
        L::Service: Service<HttpContext, Incoming, Response = Response<RespBody>, Error = DynError>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        Route::new(l.layer(self))
    }
}

impl Service<HttpContext, Incoming> for Route {
    type Response = Response<RespBody>;

    type Error = DynError;

    #[inline]
    async fn call<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        self.0.call(cx, req).await
    }
}
