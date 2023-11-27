use std::collections::HashMap;

use http::{Method, Response, StatusCode};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use motore::{layer::Layer, Service};

use crate::{
    handler::{DynHandler, Handler},
    response::RespBody,
    DynError, DynService, HttpContext,
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

pub struct Router<S = ()> {
    matcher: matchit::Router<RouteId>,
    routes: HashMap<RouteId, MethodRouter<S>>,
}

impl<S> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            matcher: Default::default(),
            routes: Default::default(),
        }
    }

    pub fn route<R>(mut self, uri: R, route: MethodRouter<S>) -> Self
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
        L: Layer<DynService> + Clone + Send + Sync + 'static,
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

    #[allow(dead_code)]
    fn with_state<S2>(self, s: S) -> Router<S2> {
        let routes = self
            .routes
            .into_iter()
            .map(|(id, route)| {
                let route = route.with_state(s.clone());
                (id, route)
            })
            .collect();

        Router {
            matcher: self.matcher,
            routes,
        }
    }
}

impl Service<HttpContext, Incoming> for Router<()> {
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
                return srv.call_with_state(cx, req, ()).await;
            }
        }

        Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::new()).into())
            .unwrap())
    }
}

pub struct MethodRouter<S = ()> {
    options: MethodEndpoint<S>,
    get: MethodEndpoint<S>,
    post: MethodEndpoint<S>,
    put: MethodEndpoint<S>,
    delete: MethodEndpoint<S>,
    head: MethodEndpoint<S>,
    trace: MethodEndpoint<S>,
    connect: MethodEndpoint<S>,
    patch: MethodEndpoint<S>,
}

impl<S> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            options: MethodEndpoint::None,
            get: MethodEndpoint::None,
            post: MethodEndpoint::None,
            put: MethodEndpoint::None,
            delete: MethodEndpoint::None,
            head: MethodEndpoint::None,
            trace: MethodEndpoint::None,
            connect: MethodEndpoint::None,
            patch: MethodEndpoint::None,
        }
    }

    pub fn builder() -> MethodRouterBuilder<S> {
        MethodRouterBuilder {
            router: Self::new(),
        }
    }

    pub fn layer<L>(self, l: L) -> Self
    where
        L: Layer<DynService> + Clone + Send + Sync + 'static,
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

        let layer_fn = move |route: DynService| DynService::new(l.clone().layer(route));

        let options = options.map(layer_fn.clone());
        let get = get.map(layer_fn.clone());
        let post = post.map(layer_fn.clone());
        let put = put.map(layer_fn.clone());
        let delete = delete.map(layer_fn.clone());
        let head = head.map(layer_fn.clone());
        let trace = trace.map(layer_fn.clone());
        let connect = connect.map(layer_fn.clone());
        let patch = patch.map(layer_fn.clone());

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

    pub fn with_state<S2>(self, state: S) -> MethodRouter<S2> {
        MethodRouter {
            options: self.options.with_state(&state),
            get: self.get.with_state(&state),
            post: self.post.with_state(&state),
            put: self.put.with_state(&state),
            delete: self.delete.with_state(&state),
            head: self.head.with_state(&state),
            trace: self.trace.with_state(&state),
            connect: self.connect.with_state(&state),
            patch: self.patch.with_state(&state),
        }
    }

    async fn call_with_state<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
        state: S,
    ) -> Result<Response<RespBody>, DynError>
    where
        S: 'cx,
    {
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
                    .body(Full::new(Bytes::new()).into())
                    .unwrap());
            }
        };

        match handler {
            MethodEndpoint::None => Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()).into())
                .unwrap()),
            MethodEndpoint::Route(route) => route.call(cx, req).await,
            MethodEndpoint::Handler(handler) => {
                handler.clone().call_with_state(cx, req, state).await
            }
        }
    }
}

macro_rules! for_all_methods {
    ($name:ident) => {
        $name!(options, get, post, put, delete, head, trace, connect, patch);
    };
}

pub struct MethodRouterBuilder<S> {
    router: MethodRouter<S>,
}

macro_rules! impl_method_register_for_builder {
    ($( $method:ident ),*) => {
        $(
        pub fn $method<H, T>(mut self, handler: H) -> Self
        where
            for<'a> H: Handler<T, S> + Clone + Send + Sync + 'a,
            for<'a> T: 'a,
        {
            self.router.$method = MethodEndpoint::Handler(DynHandler::new(handler));
            self
        }
        )+
    };
}

impl<S> MethodRouterBuilder<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            router: MethodRouter::new(),
        }
    }

    for_all_methods!(impl_method_register_for_builder);

    pub fn build(self) -> MethodRouter<S> {
        self.router
    }
}

#[derive(Clone, Default)]
enum MethodEndpoint<S> {
    #[default]
    None,
    Route(DynService),
    Handler(DynHandler<S>),
}

impl<S> MethodEndpoint<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn map<F>(self, f: F) -> MethodEndpoint<S>
    where
        F: FnOnce(DynService) -> DynService + Clone + 'static,
    {
        match self {
            MethodEndpoint::None => MethodEndpoint::None,
            MethodEndpoint::Route(route) => MethodEndpoint::Route(f(route)),
            MethodEndpoint::Handler(handler) => MethodEndpoint::Handler(handler.map(f)),
        }
    }

    fn with_state<S2>(self, state: &S) -> MethodEndpoint<S2> {
        match self {
            MethodEndpoint::None => MethodEndpoint::None,
            MethodEndpoint::Route(route) => MethodEndpoint::Route(route),
            MethodEndpoint::Handler(handler) => {
                MethodEndpoint::Route(handler.into_route(state.clone()))
            }
        }
    }
}

macro_rules! impl_method_register {
    ($( $method:ident ),*) => {
        $(
        pub fn $method<H, T, S>(h: H) -> MethodRouter<S>
        where
            for<'a> H: Handler<T, S> + Clone + Send + Sync + 'a,
            for<'a> T: 'a,
            S: Clone + Send + Sync + 'static,
        {
            MethodRouterBuilder::new().$method(h).build()
        }
        )+
    };
}

for_all_methods!(impl_method_register);
