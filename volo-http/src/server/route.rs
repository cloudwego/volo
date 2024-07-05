//! Route module for routing path to [`Service`]s or handlers.
//!
//! This module includes [`Router`], [`MethodRouter`] and [`Route`]. The call path is:
//!
//! `Router` -> `MethodRouter` -> `Route`.
//!
//! [`Router`] is the main router for routing path (uri) to [`MethodRouter`]s. [`MethodRouter`] is
//! a router for routing method (GET, POST, ...) to [`Route`]s. [`Route`] is a handler or service
//! for handling the request.

#![deny(missing_docs)]

use std::{
    collections::HashMap, convert::Infallible, error::Error, fmt, future::Future,
    marker::PhantomData, str::FromStr,
};

use http::{Method, StatusCode, Uri};
use hyper::body::Incoming;
use motore::{layer::Layer, service::Service, ServiceExt};
use paste::paste;

use super::{handler::Handler, IntoResponse};
use crate::{context::ServerContext, request::ServerRequest, response::ServerResponse};

/// The route service used for [`Router`].
pub type Route<B = Incoming, E = Infallible> =
    motore::service::BoxService<ServerContext, ServerRequest<B>, ServerResponse, E>;

// The `matchit::Router` cannot be converted to `Iterator`, so using
// `matchit::Router<MethodRouter>` is not convenient enough.
//
// To solve the problem, we refer to the implementation of `axum` and introduce a `RouteId` as a
// bridge, the `matchit::Router` only handles some IDs and each ID corresponds to a `MethodRouter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RouteId(u32);

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

/// The router for routing path to [`Service`]s or handlers.
#[must_use]
pub struct Router<B = Incoming, E = Infallible> {
    matcher: Matcher,
    routes: HashMap<RouteId, Endpoint<B, E>>,
    fallback: Fallback<B, E>,
    is_default_fallback: bool,
}

impl<B, E> Default for Router<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<B, E> Router<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    /// Create a new router.
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

    /// Create a route for the given path with the given [`MethodRouter`].
    ///
    /// The uri matcher is based on [`matchit`](https://docs.rs/matchit/0.8.0/matchit/).  It
    /// supports normal path and parameterized path.
    ///
    /// # Examples
    ///
    /// ## Normal path
    ///
    /// ```
    /// use volo_http::server::route::{get, Router};
    ///
    /// async fn index() -> &'static str {
    ///     "Hello, World"
    /// }
    ///
    /// let router: Router = Router::new().route("/", get(index));
    /// ```
    ///
    /// ## Path with Named Parameters
    ///
    /// Named parameters like `/{id}` match anything until the next `/` or the end of the path.
    ///
    /// The params can be extract by extractor `PathParamsMap`:
    ///
    /// ```
    /// use volo::FastStr;
    /// use volo_http::server::{
    ///     param::PathParamsMap,
    ///     route::{get, Router},
    /// };
    ///
    /// async fn param(map: PathParamsMap) -> FastStr {
    ///     map.get("id").unwrap().clone()
    /// }
    ///
    /// let router: Router = Router::new().route("/user/{id}", get(param));
    /// ```
    ///
    /// Or you can use `PathParams` directly:
    ///
    /// ```
    /// use volo::FastStr;
    /// use volo_http::server::{
    ///     param::PathParams,
    ///     route::{get, Router},
    /// };
    ///
    /// async fn param(PathParams(id): PathParams<String>) -> String {
    ///     id
    /// }
    ///
    /// let router: Router = Router::new().route("/user/{id}", get(param));
    /// ```
    ///
    /// More than one params are also supported:
    ///
    /// ```
    /// use volo::FastStr;
    /// use volo_http::server::{
    ///     param::PathParams,
    ///     route::{get, Router},
    /// };
    ///
    /// async fn param(PathParams((user, post)): PathParams<(usize, usize)>) -> String {
    ///     format!("user id: {user}, post id: {post}")
    /// }
    ///
    /// let router: Router = Router::new().route("/user/{user}/post/{post}", get(param));
    /// ```
    ///
    /// ## Path with Catch-all Parameters
    ///
    /// Catch-all parameters start with `*` and match anything until the end of the path. They must
    /// always be at the **end** of the route.
    ///
    /// ```
    /// use volo_http::server::{
    ///     param::PathParams,
    ///     route::{get, Router},
    /// };
    ///
    /// async fn index() -> &'static str {
    ///     "Hello, World"
    /// }
    ///
    /// async fn fallback(PathParams(uri): PathParams<String>) -> String {
    ///     format!("Path `{uri}` is not available")
    /// }
    ///
    /// let router: Router = Router::new()
    ///     .route("/", get(index))
    ///     .route("/index", get(index))
    ///     .route("/{*fallback}", get(fallback));
    /// ```
    ///
    /// For more usage methods, please refer to:
    /// [`matchit`](https://docs.rs/matchit/0.8.0/matchit/).
    pub fn route<S>(mut self, uri: S, method_router: MethodRouter<B, E>) -> Self
    where
        S: AsRef<str>,
    {
        let route_id = self
            .matcher
            .insert(uri.as_ref())
            .expect("Insert routing rule failed");

        self.routes
            .insert(route_id, Endpoint::MethodRouter(method_router));

        self
    }

    /// Create a route for the given path with a given [`Router`] and nest it into the current
    /// router.
    ///
    /// The `uri` param is a prefix of the whole uri and will be stripped before calling the inner
    /// router, and the inner [`Router`] will handle uri without the given prefix, but all params
    /// will be kept.
    ///
    /// # Examples
    ///
    /// ```
    /// use volo_http::server::{
    ///     param::PathParams,
    ///     route::{get, Router},
    /// };
    ///
    /// async fn hello_world() -> &'static str {
    ///     "Hello, World"
    /// }
    /// async fn handle_tid(PathParams(tid): PathParams<String>) -> String {
    ///     tid
    /// }
    /// async fn uid_and_tid(PathParams((uid, tid)): PathParams<(String, String)>) -> String {
    ///     format!("uid: {uid}, tid: {tid}")
    /// }
    ///
    /// let post_router = Router::new()
    ///     // http://<SERVER>/post/
    ///     .route("/", get(hello_world))
    ///     // http://<SERVER>/post/114
    ///     .route("/{tid}", get(handle_tid));
    /// let user_router = Router::new()
    ///     // http://<SERVER>/user/114/name
    ///     .route("/name", get(hello_world))
    ///     // http://<SERVER>/user/114/tid/514
    ///     .route("/post/{tid}", get(uid_and_tid));
    ///
    /// let router: Router = Router::new()
    ///     .nest("/post", post_router)
    ///     .nest("/user/{uid}/", user_router);
    /// ```
    pub fn nest<U>(self, uri: U, router: Router<B, E>) -> Self
    where
        U: AsRef<str>,
    {
        self.nest_route(uri.as_ref().to_owned(), Route::new(router))
    }

    /// Create a route for the given path with a given [`Service`] and nest it into the current
    /// router.
    ///
    /// The service will handle any uri with the param `uri` as its prefix.
    pub fn nest_service<U, S>(self, uri: U, service: S) -> Self
    where
        U: AsRef<str>,
        S: Service<ServerContext, ServerRequest<B>, Error = E> + Send + Sync + 'static,
        S::Response: IntoResponse,
    {
        self.nest_route(
            uri.as_ref().to_owned(),
            Route::new(service.map_response(IntoResponse::into_response)),
        )
    }

    fn nest_route(mut self, prefix: String, route: Route<B, E>) -> Self {
        let uri = if prefix.ends_with('/') {
            format!("{prefix}{NEST_CATCH_PARAM}")
        } else {
            format!("{prefix}/{NEST_CATCH_PARAM}")
        };

        // Because we use `matchit::Router` for matching uri, for `/{*catch}`, `/xxx` matches it
        // but `/` does not match. To solve the problem, we should also insert `/` for handling it.
        let route_id = self
            .matcher
            .insert(prefix.clone())
            .expect("Insert routing rule failed");

        // If user uses `router.nest("/user", another)`, `/user`, `/user/`, `/user/{*catch}` should
        // be inserted. But if user uses `/user/`, we will insert `/user/` and `/user/{*catch}`
        // only.
        if !prefix.ends_with('/') {
            let prefix_with_slash = prefix + "/";
            self.matcher
                .insert_with_id(prefix_with_slash, route_id)
                .expect("Insert routing rule failed");
        }

        self.matcher
            .insert_with_id(uri, route_id)
            .expect("Insert routing rule failed");

        self.routes.insert(
            route_id,
            Endpoint::Service(Route::new(StripPrefixLayer.layer(route))),
        );

        self
    }

    /// Set a global fallback for router.
    ///
    /// If there is no route matches the current uri, router will call the fallback handler.
    ///
    /// Default is returning "404 Not Found".
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        for<'a> H: Handler<T, B, E> + Clone + Send + Sync + 'a,
        T: 'static,
        E: 'static,
    {
        self.fallback = Fallback::from_handler(handler);
        self
    }

    /// Set a global fallback for router.
    ///
    /// If there is no route matches the current uri, router will call the fallback service.
    ///
    /// Default is returning "404 Not Found".
    pub fn fallback_service<S>(mut self, service: S) -> Self
    where
        for<'a> S: Service<ServerContext, ServerRequest<B>, Error = E> + Send + Sync + 'a,
        S::Response: IntoResponse,
    {
        self.fallback = Fallback::from_service(service);
        self
    }

    /// Merge another router to self.
    ///
    /// # Panics
    ///
    /// - Panics if the two router have routes with the same path.
    ///
    /// # Examples
    ///
    /// ```
    /// use volo_http::server::route::{get, Router};
    ///
    /// async fn index() -> &'static str {
    ///     "Hello, World"
    /// }
    ///
    /// fn foo_router() -> Router {
    ///     Router::new()
    ///         .route("/foo/", get(index))
    ///         .route("/foo/index", get(index))
    /// }
    ///
    /// fn bar_router() -> Router {
    ///     Router::new()
    ///         .route("/bar/", get(index))
    ///         .route("/bar/index", get(index))
    /// }
    ///
    /// fn baz_router() -> Router {
    ///     Router::new()
    ///         .route("/baz/", get(index))
    ///         .route("/baz/index", get(index))
    /// }
    ///
    /// let app = Router::new()
    ///     .merge(foo_router())
    ///     .merge(bar_router())
    ///     .merge(baz_router());
    /// ```
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
                unreachable!()
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

    /// Add a new inner layer to all routes in router.
    ///
    /// The layer's `Service` should be `Send + Sync + 'static`.
    pub fn layer<L, B2, E2>(self, l: L) -> Router<B2, E2>
    where
        L: Layer<Route<B, E>> + Clone + Send + Sync + 'static,
        L::Service: Service<ServerContext, ServerRequest<B2>, Error = E2> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest<B2>>>::Response: IntoResponse,
        B2: 'static,
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

impl<B, E> Service<ServerContext, ServerRequest<B>> for Router<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        if let Ok(matched) = self.matcher.at(req.uri().clone().path()) {
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

enum Endpoint<B = Incoming, E = Infallible> {
    MethodRouter(MethodRouter<B, E>),
    Service(Route<B, E>),
}

const NEST_CATCH_PARAM: &str = "{*__priv_nest_catch_param}";
const NEST_CATCH_PARAM_NAME: &str = "__priv_nest_catch_param";

impl<B, E> Service<ServerContext, ServerRequest<B>> for Endpoint<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::MethodRouter(mr) => mr.call(cx, req).await,
            Self::Service(service) => service.call(cx, req).await,
        }
    }
}

impl<B, E> Default for Endpoint<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    fn default() -> Self {
        Self::MethodRouter(Default::default())
    }
}

impl<B, E> Endpoint<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    fn layer<L, B2, E2>(self, l: L) -> Endpoint<B2, E2>
    where
        L: Layer<Route<B, E>> + Clone + Send + Sync + 'static,
        L::Service: Service<ServerContext, ServerRequest<B2>, Error = E2> + Send + Sync,
        <L::Service as Service<ServerContext, ServerRequest<B2>>>::Response: IntoResponse,
        B2: 'static,
    {
        match self {
            Self::MethodRouter(mr) => Endpoint::MethodRouter(mr.layer(l)),
            Self::Service(s) => Endpoint::Service(Route::new(
                l.layer(s).map_response(IntoResponse::into_response),
            )),
        }
    }
}

/// A method router that handle the request and dispatch it by its method.
///
/// There is no need to create [`MethodRouter`] directly, you can use specific method for creating
/// it. What's more, the method router allows chaining additional handlers or services.
///
/// # Examples
///
/// ```
/// use std::convert::Infallible;
///
/// use volo::service::service_fn;
/// use volo_http::{
///     context::ServerContext,
///     request::ServerRequest,
///     server::route::{any, get, post_service, MethodRouter, Router},
/// };
///
/// async fn index() -> &'static str {
///     "Hello, World"
/// }
///
/// async fn index_fn(
///     cx: &mut ServerContext,
///     req: ServerRequest,
/// ) -> Result<&'static str, Infallible> {
///     Ok("Hello, World")
/// }
///
/// let _: MethodRouter = get(index);
/// let _: MethodRouter = any(index);
/// let _: MethodRouter = post_service(service_fn(index_fn));
///
/// let _: MethodRouter = get(index).post(index).options_service(service_fn(index_fn));
///
/// let app: Router = Router::new().route("/", get(index));
/// let app: Router = Router::new().route("/", get(index).post(index).head(index));
/// ```
pub struct MethodRouter<B = Incoming, E = Infallible> {
    options: MethodEndpoint<B, E>,
    get: MethodEndpoint<B, E>,
    post: MethodEndpoint<B, E>,
    put: MethodEndpoint<B, E>,
    delete: MethodEndpoint<B, E>,
    head: MethodEndpoint<B, E>,
    trace: MethodEndpoint<B, E>,
    connect: MethodEndpoint<B, E>,
    patch: MethodEndpoint<B, E>,
    fallback: Fallback<B, E>,
}

impl<B, E> Service<ServerContext, ServerRequest<B>> for MethodRouter<B, E>
where
    B: Send,
{
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
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

impl<B, E> Default for MethodRouter<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<B, E> MethodRouter<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    fn new() -> Self {
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

    /// Add a new inner layer to all routes in this method router.
    ///
    /// The layer's `Service` should be `Clone + Send + Sync + 'static`.
    pub fn layer<L, B2, E2>(self, l: L) -> MethodRouter<B2, E2>
    where
        L: Layer<Route<B, E>> + Clone + Send + Sync + 'static,
        L::Service: Service<ServerContext, ServerRequest<B2>, Error = E2> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest<B2>>>::Response: IntoResponse,
        B2: 'static,
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

        let layer_fn = move |route: Route<B, E>| {
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
        #[doc = concat!("Route `", stringify!($method) ,"` requests to the given handler.")]
        pub fn $method<H, T>(mut self, handler: H) -> Self
        where
            for<'a> H: Handler<T, B, E> + Clone + Send + Sync + 'a,
            B: Send,
            T: 'static,
        {
            self.$method = MethodEndpoint::from_handler(handler);
            self
        }

        paste! {
        #[doc = concat!("Route `", stringify!($method) ,"` requests to the given service.")]
        pub fn [<$method _service>]<S>(mut self, service: S) -> MethodRouter<B, E>
        where
            for<'a> S: Service<ServerContext, ServerRequest<B>, Error = E>
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

impl<B, E> MethodRouter<B, E>
where
    B: Send + 'static,
    E: IntoResponse + 'static,
{
    for_all_methods!(impl_method_register_for_builder);

    /// Set a fallback handler for the route.
    ///
    /// If there is no method that the route can handle, method router will call the fallback
    /// handler.
    ///
    /// Default is returning "405 Method Not Allowed".
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        for<'a> H: Handler<T, B, E> + Clone + Send + Sync + 'a,
        T: 'static,
    {
        self.fallback = Fallback::from_handler(handler);
        self
    }

    /// Set a fallback service for the route.
    ///
    /// If there is no method that the route can handle, method router will call the fallback
    /// service.
    ///
    /// Default is returning "405 Method Not Allowed".
    pub fn fallback_service<S>(mut self, service: S) -> Self
    where
        for<'a> S: Service<ServerContext, ServerRequest<B>, Error = E> + Send + Sync + 'a,
        S::Response: IntoResponse,
    {
        self.fallback = Fallback::from_service(service);
        self
    }
}

macro_rules! impl_method_register {
    ($( $method:ident ),*) => {
        $(
        #[doc = concat!("Route `", stringify!($method) ,"` requests to the given handler.")]
        pub fn $method<H, T, B, E>(handler: H) -> MethodRouter<B, E>
        where
            for<'a> H: Handler<T, B, E> + Clone + Send + Sync + 'a,
            T: 'static,
            B: Send + 'static,
            E: IntoResponse + 'static,
        {
            MethodRouter {
                $method: MethodEndpoint::from_handler(handler),
                ..Default::default()
            }
        }

        paste! {
        #[doc = concat!("Route `", stringify!($method) ,"` requests to the given service.")]
        pub fn [<$method _service>]<S, B, E>(service: S) -> MethodRouter<B, E>
        where
            for<'a> S: Service<ServerContext, ServerRequest<B>, Error = E>
                + Send
                + Sync
                + 'a,
            S::Response: IntoResponse,
            B: Send + 'static,
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

/// Route any method to the given handler.
pub fn any<H, T, B, E>(handler: H) -> MethodRouter<B, E>
where
    for<'a> H: Handler<T, B, E> + Clone + Send + Sync + 'a,
    T: 'static,
    B: Send + 'static,
    E: IntoResponse + 'static,
{
    MethodRouter {
        fallback: Fallback::from_handler(handler),
        ..Default::default()
    }
}

/// Route any method to the given service.
pub fn any_service<S, B, E>(service: S) -> MethodRouter<B, E>
where
    for<'a> S: Service<ServerContext, ServerRequest<B>, Error = E> + Send + Sync + 'a,
    S::Response: IntoResponse,
    B: Send + 'static,
    E: IntoResponse + 'static,
{
    MethodRouter {
        fallback: Fallback::from_service(service),
        ..Default::default()
    }
}

#[derive(Default)]
enum MethodEndpoint<B = Incoming, E = Infallible> {
    #[default]
    None,
    Route(Route<B, E>),
}

impl<B, E> MethodEndpoint<B, E>
where
    B: Send + 'static,
{
    fn from_handler<H, T>(handler: H) -> Self
    where
        for<'a> H: Handler<T, B, E> + Clone + Send + Sync + 'a,
        T: 'static,
        E: 'static,
    {
        Self::from_service(handler.into_service())
    }

    fn from_service<S>(service: S) -> Self
    where
        for<'a> S: Service<ServerContext, ServerRequest<B>, Error = E> + Send + Sync + 'a,
        S::Response: IntoResponse,
    {
        Self::Route(Route::new(
            service.map_response(IntoResponse::into_response),
        ))
    }

    fn map<F, B2, E2>(self, f: F) -> MethodEndpoint<B2, E2>
    where
        F: FnOnce(Route<B, E>) -> Route<B2, E2> + Clone + 'static,
    {
        match self {
            Self::None => MethodEndpoint::None,
            Self::Route(route) => MethodEndpoint::Route(f(route)),
        }
    }
}

enum Fallback<B = Incoming, E = Infallible> {
    Route(Route<B, E>),
}

impl<B, E> Service<ServerContext, ServerRequest<B>> for Fallback<B, E>
where
    B: Send,
{
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::Route(route) => route.call(cx, req).await,
        }
    }
}

impl<B, E> Fallback<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    fn from_status_code(status: StatusCode) -> Self {
        Self::from_service(RouteForStatusCode::new(status))
    }

    fn from_handler<H, T>(handler: H) -> Self
    where
        H: Handler<T, B, E> + Clone + Send + Sync + 'static,
        T: 'static,
    {
        Self::from_service(handler.into_service())
    }

    fn from_service<S>(service: S) -> Self
    where
        S: Service<ServerContext, ServerRequest<B>, Error = E> + Send + Sync + 'static,
        S::Response: IntoResponse,
    {
        Self::Route(Route::new(
            service.map_response(IntoResponse::into_response),
        ))
    }

    fn map<F, B2, E2>(self, f: F) -> Fallback<B2, E2>
    where
        F: FnOnce(Route<B, E>) -> Route<B2, E2> + Clone + 'static,
    {
        match self {
            Self::Route(route) => Fallback::Route(f(route)),
        }
    }

    fn layer<L, B2, E2>(self, l: L) -> Fallback<B2, E2>
    where
        L: Layer<Route<B, E>> + Clone + Send + Sync + 'static,
        L::Service: Service<ServerContext, ServerRequest<B2>, Error = E2> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, ServerRequest<B2>>>::Response: IntoResponse,
        B2: 'static,
    {
        self.map(move |route: Route<B, E>| {
            Route::new(
                l.clone()
                    .layer(route)
                    .map_response(IntoResponse::into_response),
            )
        })
    }
}

struct StripPrefixLayer;

impl<S> Layer<S> for StripPrefixLayer {
    type Service = StripPrefix<S>;

    fn layer(self, inner: S) -> Self::Service {
        StripPrefix { inner }
    }
}

struct StripPrefix<S> {
    inner: S,
}

impl<S, B, E> Service<ServerContext, ServerRequest<B>> for StripPrefix<S>
where
    S: Service<ServerContext, ServerRequest<B>, Response = ServerResponse, Error = E>,
{
    type Response = ServerResponse;
    type Error = E;

    fn call(
        &self,
        cx: &mut ServerContext,
        mut req: ServerRequest<B>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        let mut uri = String::from("/");
        if cx
            .params()
            .last()
            .is_some_and(|(k, _)| k == NEST_CATCH_PARAM_NAME)
        {
            uri += cx.params_mut().pop().unwrap().1.as_str();
        };

        // SAFETY: The value is from a valid uri, so it can also be converted into
        // a valid uri safely.
        *req.uri_mut() = Uri::from_str(&uri).unwrap();
        self.inner.call(cx, req)
    }
}

struct RouteForStatusCode<B, E> {
    status: StatusCode,
    _marker: PhantomData<fn(B, E)>,
}

impl<B, E> Clone for RouteForStatusCode<B, E> {
    fn clone(&self) -> Self {
        Self {
            status: self.status,
            _marker: self._marker,
        }
    }
}

impl<B, E> RouteForStatusCode<B, E> {
    fn new(status: StatusCode) -> Self {
        Self {
            status,
            _marker: PhantomData,
        }
    }
}

impl<B, E> Service<ServerContext, ServerRequest<B>> for RouteForStatusCode<B, E>
where
    B: Send,
{
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        _: &mut ServerContext,
        _: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.status.into_response())
    }
}

#[cfg(test)]
mod route_tests {
    use faststr::FastStr;
    use http::{method::Method, status::StatusCode, uri::Uri};

    use super::{any, get, head, options, MethodRouter};
    use crate::{
        body::{Body, BodyConversion},
        server::{param::PathParamsVec, test_helpers::TestServer},
        Router, Server,
    };

    async fn always_ok() {}
    async fn teapot() -> StatusCode {
        StatusCode::IM_A_TEAPOT
    }

    #[tokio::test]
    async fn method_router() {
        async fn test_all_method<F>(router: MethodRouter<Option<Body>>, filter: F)
        where
            F: Fn(Method) -> bool,
        {
            let methods = [
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::HEAD,
                Method::OPTIONS,
                Method::CONNECT,
                Method::PATCH,
                Method::TRACE,
            ];
            for m in methods {
                assert_eq!(
                    router
                        .call_route(m.clone(), None)
                        .await
                        .status()
                        .is_success(),
                    filter(m)
                );
            }
        }

        test_all_method(get(always_ok), |m| m == Method::GET).await;
        test_all_method(head(always_ok), |m| m == Method::HEAD).await;
        test_all_method(any(always_ok), |_| true).await;
    }

    #[tokio::test]
    async fn method_fallback() {
        async fn test_all_method<F>(router: MethodRouter<Option<Body>>, filter: F)
        where
            F: Fn(Method) -> bool,
        {
            let methods = [
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::HEAD,
                Method::OPTIONS,
                Method::CONNECT,
                Method::PATCH,
                Method::TRACE,
            ];
            for m in methods {
                assert_eq!(
                    router.call_route(m.clone(), None).await.status() == StatusCode::IM_A_TEAPOT,
                    filter(m)
                );
            }
        }

        test_all_method(get(always_ok).fallback(teapot), |m| m != Method::GET).await;
        test_all_method(options(always_ok).fallback(teapot), |m| {
            m != Method::OPTIONS
        })
        .await;
        test_all_method(any(teapot), |_| true).await;
    }

    #[tokio::test]
    async fn url_match() {
        async fn is_ok(server: &TestServer<Router<Option<Body>>, Option<Body>>, uri: &str) -> bool {
            server.call_route(Method::GET, uri, None).await.status() == StatusCode::OK
        }
        let router: Router<Option<Body>> = Router::new()
            .route("/", any(always_ok))
            .route("/catch/{id}", any(always_ok))
            .route("/catch/{id}/another", any(always_ok))
            .route("/catch/{id}/another/{uid}", any(always_ok))
            .route("/catch/{id}/another/{uid}/again", any(always_ok))
            .route("/catch/{id}/another/{uid}/again/{tid}", any(always_ok))
            .route("/catch_all/{*all}", any(always_ok));
        let server = Server::new(router).into_test_server();

        assert!(is_ok(&server, "/").await);
        assert!(is_ok(&server, "/catch/114").await);
        assert!(is_ok(&server, "/catch/514").await);
        assert!(is_ok(&server, "/catch/ll45l4").await);
        assert!(is_ok(&server, "/catch/ll45l4/another").await);
        assert!(is_ok(&server, "/catch/ll45l4/another/1919").await);
        assert!(is_ok(&server, "/catch/ll45l4/another/1919/again").await);
        assert!(is_ok(&server, "/catch/ll45l4/another/1919/again/810").await);
        assert!(is_ok(&server, "/catch_all/114").await);
        assert!(is_ok(&server, "/catch_all/114/514/1919/810").await);

        assert!(!is_ok(&server, "/catch").await);
        assert!(!is_ok(&server, "/catch/114/").await);
        assert!(!is_ok(&server, "/catch/114/another/514/").await);
        assert!(!is_ok(&server, "/catch/11/another/45/again/14/").await);
        assert!(!is_ok(&server, "/catch_all").await);
        assert!(!is_ok(&server, "/catch_all/").await);
    }

    #[tokio::test]
    async fn router_fallback() {
        async fn is_teapot(
            server: &TestServer<Router<Option<Body>>, Option<Body>>,
            uri: &str,
        ) -> bool {
            server.call_route(Method::GET, uri, None).await.status() == StatusCode::IM_A_TEAPOT
        }
        let router: Router<Option<Body>> = Router::new()
            .route("/", any(always_ok))
            .route("/catch/{id}", any(always_ok))
            .route("/catch_all/{*all}", any(always_ok))
            .fallback(teapot);
        let server = Server::new(router).into_test_server();

        assert!(is_teapot(&server, "//").await);
        assert!(is_teapot(&server, "/catch/").await);
        assert!(is_teapot(&server, "/catch_all/").await);

        assert!(!is_teapot(&server, "/catch/114").await);
        assert!(!is_teapot(&server, "/catch_all/514").await);
        assert!(!is_teapot(&server, "/catch_all/114/514/1919/810").await);
    }

    #[tokio::test]
    async fn nest_router() {
        async fn uri_and_params(uri: Uri, params: PathParamsVec) -> String {
            let mut v = vec![FastStr::from_string(uri.to_string())];
            v.extend(params.into_iter().map(|(_, v)| v));
            v.join("\n")
        }
        async fn get_res(
            server: &TestServer<Router<Option<Body>>, Option<Body>>,
            uri: &str,
        ) -> String {
            server
                .call_route(Method::GET, uri, None)
                .await
                .into_string()
                .await
                .unwrap()
        }

        let router: Router<Option<Body>> = Router::new()
            .nest(
                // uri prefix without final slash ('/')
                "/test-1",
                Router::new()
                    .route("/", any(uri_and_params))
                    .route("/id/{id}", any(uri_and_params))
                    .route("/catch/{*content}", any(uri_and_params)),
            )
            .nest(
                // uri prefix with final slash ('/')
                "/test-2/",
                Router::new()
                    .route("/", any(uri_and_params))
                    .route("/id/{id}", any(uri_and_params))
                    .route("/catch/{*content}", any(uri_and_params)),
            )
            .nest(
                // uri prefix with a param without final slash ('/')
                "/test-3/{catch}",
                Router::new()
                    .route("/", any(uri_and_params))
                    .route("/id/{id}", any(uri_and_params))
                    .route("/catch/{*content}", any(uri_and_params)),
            )
            .nest(
                // uri prefix with a param and final slash ('/')
                "/test-4/{catch}/",
                Router::new()
                    .route("/", any(uri_and_params))
                    .route("/id/{id}", any(uri_and_params))
                    .route("/catch/{*content}", any(uri_and_params)),
            );
        let server = Server::new(router).into_test_server();

        // We register it as `/test-1`, so it does match.
        assert_eq!(get_res(&server, "/test-1").await, "/");
        assert_eq!(get_res(&server, "/test-1/").await, "/");
        assert_eq!(get_res(&server, "/test-1/id/114").await, "/id/114\n114");
        assert_eq!(
            get_res(&server, "/test-1/catch/114/514/1919/810").await,
            "/catch/114/514/1919/810\n114/514/1919/810"
        );

        // We register it as `/test-2/`, so `/test-2` does not match, but `/test-2/` does match.
        assert!(get_res(&server, "/test-2").await.is_empty());
        assert_eq!(get_res(&server, "/test-2/").await, "/");
        assert_eq!(get_res(&server, "/test-2/id/114").await, "/id/114\n114");
        assert_eq!(
            get_res(&server, "/test-2/catch/114/514/1919/810").await,
            "/catch/114/514/1919/810\n114/514/1919/810"
        );

        // The first param can be kept.
        assert_eq!(get_res(&server, "/test-3/114").await, "/\n114");
        assert_eq!(get_res(&server, "/test-3/114/").await, "/\n114");
        assert_eq!(
            get_res(&server, "/test-3/114/id/514").await,
            "/id/514\n114\n514"
        );
        assert_eq!(
            get_res(&server, "/test-3/114/catch/514/1919/810").await,
            "/catch/514/1919/810\n114\n514/1919/810"
        );

        // It is also empty.
        assert!(get_res(&server, "/test-4/114").await.is_empty());
        assert_eq!(get_res(&server, "/test-4/114/").await, "/\n114");
        assert_eq!(
            get_res(&server, "/test-4/114/id/514").await,
            "/id/514\n114\n514"
        );
        assert_eq!(
            get_res(&server, "/test-4/114/catch/514/1919/810").await,
            "/catch/514/1919/810\n114\n514/1919/810"
        );
    }
}
