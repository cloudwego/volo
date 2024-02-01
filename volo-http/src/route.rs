use std::{collections::HashMap, convert::Infallible};

use http::{Method, StatusCode};
use hyper::body::Incoming;
use motore::{layer::Layer, service::Service};

use crate::{
    context::ServerContext, handler::Handler, response::IntoResponse, DynService, Response,
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

pub struct Router {
    matcher: Matcher,
    routes: HashMap<RouteId, MethodRouter>,
    fallback: Fallback,
    is_default_fallback: bool,
}

impl Default for Router {
    fn default() -> Self {
        Self {
            matcher: Default::default(),
            routes: Default::default(),
            fallback: Fallback::from_status_code(StatusCode::NOT_FOUND),
            is_default_fallback: true,
        }
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
        let route_id = self
            .matcher
            .insert(uri)
            .expect("Insert routing rule failed");

        self.routes.insert(route_id, route);

        self
    }

    pub fn fallback_for_all(mut self, fallback: Fallback) -> Self {
        self.fallback = fallback;
        self
    }

    pub fn merge(mut self, other: Router) -> Self {
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
        L::Service: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
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

        let fallback = self.fallback.layer(l.clone());

        Router {
            matcher: self.matcher,
            routes,
            fallback,
            is_default_fallback: self.is_default_fallback,
        }
    }
}

impl Service<ServerContext, Incoming> for Router {
    type Response = Response;
    type Error = Infallible;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        if let Ok(matched) = self.matcher.at(cx.uri.clone().path()) {
            if let Some(route) = self.routes.get(matched.value) {
                cx.params_mut().extend(matched.params);
                return route.call(cx, req).await;
            }
        }

        self.fallback.call(cx, req).await
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
        self.router
            .insert(uri, route_id)
            .map_err(MatcherError::RouterInsertError)?;
        Ok(())
    }

    fn at<'a>(&'a self, path: &'a str) -> Result<matchit::Match<&RouteId>, MatcherError> {
        self.router.at(path).map_err(MatcherError::RouterMatchError)
    }
}

// The fields may be warned by compiler with "field `0` is never read", but those fields will be
// used in `expect` with `Debug`. To fix the warning, just allow it.
#[allow(dead_code)]
#[derive(Debug)]
enum MatcherError {
    UriConflict(String),
    RouterInsertError(matchit::InsertError),
    RouterMatchError(matchit::MatchError),
}

pub struct MethodRouter {
    options: MethodEndpoint,
    get: MethodEndpoint,
    post: MethodEndpoint,
    put: MethodEndpoint,
    delete: MethodEndpoint,
    head: MethodEndpoint,
    trace: MethodEndpoint,
    connect: MethodEndpoint,
    patch: MethodEndpoint,
    fallback: Fallback,
}

impl Default for MethodRouter {
    fn default() -> Self {
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
}

impl Service<ServerContext, Incoming> for MethodRouter {
    type Response = Response;
    type Error = Infallible;

    async fn call(&self, cx: &mut ServerContext, req: Incoming) -> Result<Response, Infallible> {
        let handler = match cx.method() {
            &Method::OPTIONS => Some(&self.options),
            &Method::GET => Some(&self.get),
            &Method::POST => Some(&self.post),
            &Method::PUT => Some(&self.put),
            &Method::DELETE => Some(&self.delete),
            &Method::HEAD => Some(&self.head),
            &Method::TRACE => Some(&self.trace),
            &Method::CONNECT => Some(&self.connect),
            &Method::PATCH => Some(&self.patch),
            _ => None,
        };

        match handler {
            Some(MethodEndpoint::Route(route)) => route.call(cx, req).await,
            _ => self.fallback.call(cx, req).await,
        }
    }
}

impl MethodRouter {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn builder() -> MethodRouterBuilder {
        Default::default()
    }

    pub fn layer<L>(self, l: L) -> Self
    where
        L: Layer<DynService> + Clone + Send + Sync + 'static,
        L::Service: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
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
}

macro_rules! for_all_methods {
    ($name:ident) => {
        $name!(options, get, post, put, delete, head, trace, connect, patch);
    };
}

#[derive(Default)]
pub struct MethodRouterBuilder {
    router: MethodRouter,
}

macro_rules! impl_method_register_for_builder {
    ($( $method:ident ),*) => {
        $(
        pub fn $method(mut self, ep: MethodEndpoint) -> Self {
            self.router.$method = ep;
            self
        }
        )+
    };
}

impl MethodRouterBuilder {
    pub fn new() -> Self {
        Default::default()
    }

    for_all_methods!(impl_method_register_for_builder);

    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        for<'a> H: Handler<T> + Clone + Send + Sync + 'a,
        for<'a> T: 'a,
    {
        self.router.fallback = Fallback::from_handler(handler);
        self
    }

    pub fn build(self) -> MethodRouter {
        self.router
    }
}

macro_rules! impl_method_register {
    ($( $method:ident ),*) => {
        $(
        pub fn $method<H, T>(handler: H) -> MethodRouter
        where
            for<'a> H: Handler<T> + Clone + Send + Sync + 'a,
            for<'a> T: 'a,
        {
            MethodRouterBuilder::new().$method(MethodEndpoint::from_handler(handler)).build()
        }
        )+
    };
}

for_all_methods!(impl_method_register);

pub fn any<H, T>(handler: H) -> MethodRouter
where
    for<'a> H: Handler<T> + Clone + Send + Sync + 'a,
    for<'a> T: 'a,
{
    MethodRouterBuilder::new().fallback(handler).build()
}

#[derive(Default)]
pub enum MethodEndpoint {
    #[default]
    None,
    Route(DynService),
}

impl MethodEndpoint {
    pub fn from_handler<H, T>(handler: H) -> Self
    where
        for<'a> H: Handler<T> + Clone + Send + Sync + 'a,
        for<'a> T: 'a,
    {
        Self::from_service(handler.into_service())
    }

    pub fn from_service<S>(service: S) -> Self
    where
        S: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        Self::Route(DynService::new(service))
    }

    pub(crate) fn map<F>(self, f: F) -> Self
    where
        F: FnOnce(DynService) -> DynService + Clone + 'static,
    {
        match self {
            Self::None => Self::None,
            Self::Route(route) => Self::Route(f(route)),
        }
    }
}

pub enum Fallback {
    Route(DynService),
}

impl Default for Fallback {
    fn default() -> Self {
        Self::from_status_code(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl Service<ServerContext, Incoming> for Fallback {
    type Response = Response;
    type Error = Infallible;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Route(route) => route.call(cx, req).await,
        }
    }
}

impl Fallback {
    pub(crate) fn from_status_code(status: StatusCode) -> Self {
        Self::from_service(RouteForStatusCode(status))
    }

    pub fn from_handler<H, T>(handler: H) -> Self
    where
        for<'a> H: Handler<T> + Clone + Send + Sync + 'a,
        for<'a> T: 'a,
    {
        Self::from_service(handler.into_service())
    }

    pub fn from_service<S>(service: S) -> Self
    where
        S: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        Self::Route(DynService::new(service))
    }

    pub(crate) fn map<F>(self, f: F) -> Self
    where
        F: FnOnce(DynService) -> DynService + Clone + 'static,
    {
        match self {
            Self::Route(route) => Self::Route(f(route)),
        }
    }

    pub(crate) fn layer<L>(self, l: L) -> Self
    where
        L: Layer<DynService> + Clone + Send + Sync + 'static,
        L::Service: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        self.map(move |route: DynService| DynService::new(l.clone().layer(route)))
    }
}

pub fn from_handler<H, T>(handler: H) -> MethodEndpoint
where
    for<'a> H: Handler<T> + Clone + Send + Sync + 'a,
    for<'a> T: 'a,
{
    MethodEndpoint::from_handler(handler)
}

pub fn from_service<S>(service: S) -> MethodEndpoint
where
    S: Service<ServerContext, Incoming, Response = Response, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
{
    MethodEndpoint::from_service(service)
}

pub fn service_fn<F>(f: F) -> MethodEndpoint
where
    F: for<'r> crate::service_fn::Callback<'r> + Clone + Send + Sync + 'static,
{
    MethodEndpoint::from_service(crate::service_fn::service_fn(f))
}

#[derive(Clone)]
struct RouteForStatusCode(StatusCode);

impl Service<ServerContext, Incoming> for RouteForStatusCode {
    type Response = Response;
    type Error = Infallible;

    async fn call<'s, 'cx>(
        &'s self,
        _cx: &'cx mut ServerContext,
        _req: Incoming,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.0.into_response())
    }
}
