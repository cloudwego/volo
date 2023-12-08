use std::{collections::HashMap, convert::Infallible};

use hyper::{
    body::Incoming,
    http::{Method, StatusCode},
};
use motore::{layer::Layer, service::Service};

use crate::{
    handler::{DynHandler, Handler},
    response::IntoResponse,
    DynService, HttpContext, Response,
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
    matcher: Matcher,
    routes: HashMap<RouteId, MethodRouter<S>>,
    fallback: Fallback<S>,
    is_default_fallback: bool,
}

impl<S> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            matcher: Default::default(),
            routes: Default::default(),
            fallback: Fallback::from_status_code(StatusCode::NOT_FOUND),
            is_default_fallback: true,
        }
    }

    pub fn route<R>(mut self, uri: R, route: MethodRouter<S>) -> Self
    where
        R: Into<String>,
    {
        let route_id = self
            .matcher
            .insert(uri)
            .expect("Insert routing rule failed");

        self.routes.insert(route_id, route);

        self
    }

    pub fn fallback_for_all(mut self, fallback: Fallback<S>) -> Self {
        self.fallback = fallback;
        self
    }

    pub fn merge(mut self, other: Router<S>) -> Self {
        let Router {
            mut matcher,
            mut routes,
            fallback,
            is_default_fallback,
        } = other;

        for (path, route_id) in matcher.matches.drain() {
            self.matcher
                .insert_with_id(path, route_id)
                .expect("Insert routing rule failed during merging router");
        }
        for (route_id, method_router) in routes.drain() {
            if self.routes.insert(route_id, method_router).is_some() {
                // Infallible
                panic!(
                    "Insert routes failed during merging router: Conflicting `RouteId`: \
                     {route_id:?}"
                );
            }
        }

        match (self.is_default_fallback, is_default_fallback) {
            (_, true) => {}
            (true, false) => {
                self.fallback = fallback;
                self.is_default_fallback = false;
            }
            (false, false) => {
                panic!("Merge `Router` failed because both `Router` have customized `fallback`")
            }
        }

        self
    }

    pub fn layer<L>(self, l: L) -> Self
    where
        L: Layer<DynService> + Clone + Send + Sync + 'static,
        L::Service: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<HttpContext, Incoming>>::Response: Send + 'static,
        <L::Service as Service<HttpContext, Incoming>>::Error: Send + 'static,
    {
        let routes = self
            .routes
            .into_iter()
            .map(|(id, route)| {
                let route = route.layer(l.clone());
                (id, route)
            })
            .collect();

        let fallback = self.fallback.layer(l.clone());

        Router {
            matcher: self.matcher,
            routes,
            fallback,
            is_default_fallback: self.is_default_fallback,
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

        let fallback = self.fallback.with_state(&s);

        Router {
            matcher: self.matcher,
            routes,
            fallback,
            is_default_fallback: self.is_default_fallback,
        }
    }
}

impl Service<HttpContext, Incoming> for Router<()> {
    type Response = Response;

    type Error = Infallible;

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

        self.fallback.call_with_state(cx, req, ()).await
    }
}

#[derive(Default)]
struct Matcher {
    matches: HashMap<String, RouteId>,
    router: matchit::Router<RouteId>,
}

impl Matcher {
    fn insert<R>(&mut self, uri: R) -> Result<RouteId, MatcherError>
    where
        R: Into<String>,
    {
        let route_id = RouteId::next();
        self.insert_with_id(uri, route_id)?;
        Ok(route_id)
    }

    fn insert_with_id<R>(&mut self, uri: R, route_id: RouteId) -> Result<(), MatcherError>
    where
        R: Into<String>,
    {
        let uri = uri.into();
        if self.matches.insert(uri.clone(), route_id).is_some() {
            return Err(MatcherError::UriConflict(uri));
        }
        let _ = self
            .router
            .insert(uri, route_id)
            .map_err(MatcherError::RouterInsertError)?;
        Ok(())
    }

    fn at<'a>(&'a self, path: &'a str) -> Result<matchit::Match<&RouteId>, MatcherError> {
        self.router.at(path).map_err(MatcherError::RouterMatchError)
    }
}

#[derive(Debug)]
enum MatcherError {
    UriConflict(String),
    RouterInsertError(matchit::InsertError),
    RouterMatchError(matchit::MatchError),
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
    fallback: Fallback<S>,
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
            fallback: Fallback::from_status_code(StatusCode::METHOD_NOT_ALLOWED),
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
        L::Service: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
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
            fallback,
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

        let fallback = fallback.map(layer_fn);

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
            fallback,
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
            fallback: self.fallback.with_state(&state),
        }
    }

    pub(crate) async fn call_with_state<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
        state: S,
    ) -> Result<Response, Infallible>
    where
        S: 'cx,
    {
        let handler = match cx.method {
            Method::OPTIONS => Some(&self.options),
            Method::GET => Some(&self.get),
            Method::POST => Some(&self.post),
            Method::PUT => Some(&self.put),
            Method::DELETE => Some(&self.delete),
            Method::HEAD => Some(&self.head),
            Method::TRACE => Some(&self.trace),
            Method::CONNECT => Some(&self.connect),
            Method::PATCH => Some(&self.patch),
            _ => None,
        };

        match handler {
            Some(MethodEndpoint::Route(route)) => route.call(cx, req).await,
            Some(MethodEndpoint::Handler(handler)) => {
                handler.clone().call_with_state(cx, req, state).await
            }
            _ => self.fallback.call_with_state(cx, req, state).await,
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

#[derive(Clone, Default)]
pub enum MethodEndpoint<S> {
    #[default]
    None,
    Route(DynService),
    Handler(DynHandler<S>),
}

impl<S> MethodEndpoint<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn from_handler<H, T>(h: H) -> MethodEndpoint<S>
    where
        for<'a> H: Handler<T, S> + Clone + Send + Sync + 'a,
        for<'a> T: 'a,
        S: Clone + Send + Sync + 'static,
    {
        MethodEndpoint::Handler(DynHandler::new(h))
    }

    pub fn from_service<Srv>(srv: Srv) -> MethodEndpoint<S>
    where
        Srv: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        MethodEndpoint::Route(DynService::new(srv))
    }

    pub(crate) fn map<F>(self, f: F) -> MethodEndpoint<S>
    where
        F: FnOnce(DynService) -> DynService + Clone + 'static,
    {
        match self {
            MethodEndpoint::None => MethodEndpoint::None,
            MethodEndpoint::Route(route) => MethodEndpoint::Route(f(route)),
            MethodEndpoint::Handler(handler) => MethodEndpoint::Handler(handler.map(f)),
        }
    }

    pub(crate) fn with_state<S2>(self, state: &S) -> MethodEndpoint<S2> {
        match self {
            MethodEndpoint::None => MethodEndpoint::None,
            MethodEndpoint::Route(route) => MethodEndpoint::Route(route),
            MethodEndpoint::Handler(handler) => {
                MethodEndpoint::Route(handler.into_route(state.clone()))
            }
        }
    }
}

#[derive(Clone)]
pub enum Fallback<S> {
    Route(DynService),
    Handler(DynHandler<S>),
}

impl<S> Fallback<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub(crate) fn from_status_code(status: StatusCode) -> Fallback<S> {
        Fallback::Route(DynService::new(RouteForStatusCode(status)))
    }

    pub fn from_handler<H, T>(h: H) -> Fallback<S>
    where
        for<'a> H: Handler<T, S> + Clone + Send + Sync + 'a,
        for<'a> T: 'a,
        S: Clone + Send + Sync + 'static,
    {
        Fallback::Handler(DynHandler::new(h))
    }

    pub fn from_service<Srv>(srv: Srv) -> Fallback<S>
    where
        Srv: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        Fallback::Route(DynService::new(srv))
    }

    pub(crate) fn map<F>(self, f: F) -> Fallback<S>
    where
        F: FnOnce(DynService) -> DynService + Clone + 'static,
    {
        match self {
            Fallback::Route(route) => Fallback::Route(f(route)),
            Fallback::Handler(handler) => Fallback::Handler(handler.map(f)),
        }
    }

    pub(crate) fn layer<L>(self, l: L) -> Self
    where
        L: Layer<DynService> + Clone + Send + Sync + 'static,
        L::Service: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        self.map(move |route: DynService| DynService::new(l.clone().layer(route)))
    }

    pub(crate) fn with_state<S2>(self, state: &S) -> Fallback<S2> {
        match self {
            Fallback::Route(route) => Fallback::Route(route),
            Fallback::Handler(handler) => Fallback::Route(handler.into_route(state.clone())),
        }
    }

    pub(crate) async fn call_with_state<'s, 'cx>(
        &'s self,
        cx: &'cx mut HttpContext,
        req: Incoming,
        state: S,
    ) -> Result<Response, Infallible>
    where
        S: 'cx,
    {
        match self {
            Fallback::Route(route) => route.call(cx, req).await,
            Fallback::Handler(handler) => handler.clone().call_with_state(cx, req, state).await,
        }
    }
}

pub fn from_handler<H, T, S>(h: H) -> MethodEndpoint<S>
where
    for<'a> H: Handler<T, S> + Clone + Send + Sync + 'a,
    for<'a> T: 'a,
    S: Clone + Send + Sync + 'static,
{
    MethodEndpoint::from_handler(h)
}

pub fn from_service<Srv, S>(srv: Srv) -> MethodEndpoint<S>
where
    Srv: Service<HttpContext, Incoming, Response = Response, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    S: Clone + Send + Sync + 'static,
{
    MethodEndpoint::from_service(srv)
}

#[derive(Clone)]
struct RouteForStatusCode(StatusCode);

impl Service<HttpContext, Incoming> for RouteForStatusCode {
    type Response = Response;
    type Error = Infallible;

    async fn call<'s, 'cx>(
        &'s self,
        _cx: &'cx mut HttpContext,
        _req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.0.into_response())
    }
}
