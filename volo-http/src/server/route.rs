use std::{collections::HashMap, convert::Infallible, error::Error, fmt, marker::PhantomData};

use http::{Method, StatusCode};
use motore::{layer::Layer, service::Service, ServiceExt};
use paste::paste;

use super::{handler::Handler, IntoResponse};
use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

pub type Route<E = Infallible> =
    motore::service::BoxCloneService<ServerContext, ServerRequest, ServerResponse, E>;

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

pub struct Router<E = Infallible> {
    matcher: Matcher,
    routes: HashMap<RouteId, MethodRouter<E>>,
    fallback: Fallback<E>,
    is_default_fallback: bool,
}

impl<E> Default for Router<E>
where
    E: 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Router<E> {
    pub fn new() -> Self
    where
        E: 'static,
    {
        Self {
            matcher: Default::default(),
            routes: Default::default(),
            fallback: Fallback::from_status_code(StatusCode::NOT_FOUND),
            is_default_fallback: true,
        }
    }

    pub fn route<R>(mut self, uri: R, route: MethodRouter<E>) -> Self
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

    pub fn fallback_for_all(mut self, fallback: Fallback<E>) -> Self {
        self.fallback = fallback;
        self
    }

    pub fn merge(mut self, other: Self) -> Self {
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

    pub fn layer<L, E2>(self, l: L) -> Router<E2>
    where
        L: Layer<Route<E>> + Clone + Send + Sync + 'static,
        L::Service:
            Service<ServerContext, ServerRequest, Error = E2> + Clone + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest>>::Response: IntoResponse,
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

impl Service<ServerContext, ServerRequest> for Router {
    type Response = ServerResponse;
    type Error = Infallible;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        if let Ok(matched) = self.matcher.at(req.uri().clone().path()) {
            if let Some(route) = self.routes.get(matched.value) {
                cx.params_mut().extend(matched.params);
                return Ok(route.call(cx, req).await.into_response());
            }
        }

        Ok(self.fallback.call(cx, req).await.into_response())
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

#[derive(Debug)]
enum MatcherError {
    UriConflict(String),
    RouterInsertError(matchit::InsertError),
    RouterMatchError(matchit::MatchError),
}

impl fmt::Display for MatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UriConflict(uri) => write!(f, "URI conflict: {uri}"),
            Self::RouterInsertError(err) => write!(f, "router insert error: {err}"),
            Self::RouterMatchError(err) => write!(f, "router match error: {err}"),
        }
    }
}

impl Error for MatcherError {}

pub struct MethodRouter<E = Infallible> {
    options: MethodEndpoint<E>,
    get: MethodEndpoint<E>,
    post: MethodEndpoint<E>,
    put: MethodEndpoint<E>,
    delete: MethodEndpoint<E>,
    head: MethodEndpoint<E>,
    trace: MethodEndpoint<E>,
    connect: MethodEndpoint<E>,
    patch: MethodEndpoint<E>,
    fallback: Fallback<E>,
}

impl<E> Service<ServerContext, ServerRequest> for MethodRouter<E> {
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        let handler = match *req.method() {
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
            _ => self.fallback.call(cx, req).await,
        }
    }
}

impl<E> Default for MethodRouter<E>
where
    E: 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<E> MethodRouter<E> {
    pub fn new() -> Self
    where
        E: 'static,
    {
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

    pub fn layer<L, E2>(self, l: L) -> MethodRouter<E2>
    where
        L: Layer<Route<E>> + Clone + Send + Sync + 'static,
        L::Service:
            Service<ServerContext, ServerRequest, Error = E2> + Clone + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest>>::Response: IntoResponse,
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

        let layer_fn = move |route: Route<E>| {
            Route::new(
                l.clone()
                    .layer(route)
                    .map_response(IntoResponse::into_response),
            )
        };

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

        MethodRouter {
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

macro_rules! impl_method_register_for_builder {
    ($( $method:ident ),*) => {
        $(
        pub fn $method<H, T>(mut self, handler: H) -> Self
        where
            for<'a> H: Handler<T, E> + Clone + Send + Sync + 'a,
            T: 'static,
        {
            self.$method = MethodEndpoint::from_handler(handler);
            self
        }

        paste! {
        pub fn [<$method _service>]<S>(mut self, service: S) -> MethodRouter<E>
        where
            for<'a> S: Service<ServerContext, ServerRequest, Error = E>
                + Clone
                + Send
                + Sync
                + 'a,
            S::Response: IntoResponse,
        {
            self.$method = MethodEndpoint::from_service(service);
            self
        }
        }
        )+
    };
}

impl<E> MethodRouter<E>
where
    E: IntoResponse + 'static,
{
    for_all_methods!(impl_method_register_for_builder);

    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        for<'a> H: Handler<T, E> + Clone + Send + Sync + 'a,
        T: 'static,
    {
        self.fallback = Fallback::from_handler(handler);
        self
    }

    pub fn fallback_service<S>(mut self, service: S) -> Self
    where
        for<'a> S: Service<ServerContext, ServerRequest, Error = E> + Clone + Send + Sync + 'a,
        S::Response: IntoResponse,
    {
        self.fallback = Fallback::from_service(service);
        self
    }
}

macro_rules! impl_method_register {
    ($( $method:ident ),*) => {
        $(
        pub fn $method<H, T, E>(handler: H) -> MethodRouter<E>
        where
            for<'a> H: Handler<T, E> + Clone + Send + Sync + 'a,
            T: 'static,
            E: IntoResponse + 'static,
        {
            MethodRouter {
                $method: MethodEndpoint::from_handler(handler),
                ..Default::default()
            }
        }

        paste! {
        pub fn [<$method _service>]<S, E>(service: S) -> MethodRouter<E>
        where
            for<'a> S: Service<ServerContext, ServerRequest, Error = E>
                + Clone
                + Send
                + Sync
                + 'a,
            S::Response: IntoResponse,
            E: IntoResponse + 'static,
        {
            MethodRouter {
                $method: MethodEndpoint::from_service(service),
                ..Default::default()
            }
        }
        }
        )+
    };
}

for_all_methods!(impl_method_register);

pub fn any<H, T, E>(handler: H) -> MethodRouter<E>
where
    for<'a> H: Handler<T, E> + Clone + Send + Sync + 'a,
    T: 'static,
    E: IntoResponse + 'static,
{
    MethodRouter {
        fallback: Fallback::from_handler(handler),
        ..Default::default()
    }
}

#[derive(Default)]
pub enum MethodEndpoint<E = Infallible> {
    #[default]
    None,
    Route(Route<E>),
}

impl<E> MethodEndpoint<E> {
    pub fn from_handler<H, T>(handler: H) -> Self
    where
        for<'a> H: Handler<T, E> + Clone + Send + Sync + 'a,
        T: 'static,
        E: 'static,
    {
        Self::from_service(handler.into_service())
    }

    pub fn from_service<S>(service: S) -> Self
    where
        for<'a> S: Service<ServerContext, ServerRequest, Error = E> + Clone + Send + Sync + 'a,
        S::Response: IntoResponse,
    {
        Self::Route(Route::new(
            service.map_response(IntoResponse::into_response),
        ))
    }

    pub(crate) fn map<F, E2>(self, f: F) -> MethodEndpoint<E2>
    where
        F: FnOnce(Route<E>) -> Route<E2> + Clone + 'static,
    {
        match self {
            Self::None => MethodEndpoint::None,
            Self::Route(route) => MethodEndpoint::Route(f(route)),
        }
    }
}

pub enum Fallback<E = Infallible> {
    Route(Route<E>),
}

impl<E> Service<ServerContext, ServerRequest> for Fallback<E> {
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Route(route) => route.call(cx, req).await,
        }
    }
}

impl<E> Fallback<E> {
    pub(crate) fn from_status_code(status: StatusCode) -> Self
    where
        E: 'static,
    {
        Self::from_service(RouteForStatusCode::new(status))
    }

    pub fn from_handler<H, T>(handler: H) -> Self
    where
        for<'a> H: Handler<T, E> + Clone + Send + Sync + 'a,
        T: 'static,
        E: 'static,
    {
        Self::from_service(handler.into_service())
    }

    pub fn from_service<S>(service: S) -> Self
    where
        for<'a> S: Service<ServerContext, ServerRequest, Error = E> + Clone + Send + Sync + 'a,
        S::Response: IntoResponse,
    {
        Self::Route(Route::new(
            service.map_response(IntoResponse::into_response),
        ))
    }

    pub(crate) fn map<F, E2>(self, f: F) -> Fallback<E2>
    where
        F: FnOnce(Route<E>) -> Route<E2> + Clone + 'static,
    {
        match self {
            Self::Route(route) => Fallback::Route(f(route)),
        }
    }

    pub(crate) fn layer<L, E2>(self, l: L) -> Fallback<E2>
    where
        L: Layer<Route<E>> + Clone + Send + Sync + 'static,
        L::Service:
            Service<ServerContext, ServerRequest, Error = E2> + Clone + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest>>::Response: IntoResponse,
    {
        self.map(move |route: Route<E>| {
            Route::new(
                l.clone()
                    .layer(route)
                    .map_response(IntoResponse::into_response),
            )
        })
    }
}

struct RouteForStatusCode<E> {
    status: StatusCode,
    _marker: PhantomData<fn(E)>,
}

impl<E> Clone for RouteForStatusCode<E> {
    fn clone(&self) -> Self {
        Self {
            status: self.status,
            _marker: self._marker,
        }
    }
}

impl<E> RouteForStatusCode<E> {
    fn new(status: StatusCode) -> Self {
        Self {
            status,
            _marker: PhantomData,
        }
    }
}

impl<E> Service<ServerContext, ServerRequest> for RouteForStatusCode<E> {
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        _: &mut ServerContext,
        _: ServerRequest,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.status.into_response())
    }
}
