//! [`Router`] implemententation for [`Server`].
//!
//! [`Router`] is a [`Service`] that can route a path to a [`MethodRouter`] or fallback to another
//! [`Route`].
//!
//! See [`Router`] and [`Router::route`] for more details.
//!
//! [`Server`]: crate::server::Server

use std::{collections::HashMap, convert::Infallible};

use http::status::StatusCode;
use motore::{ServiceExt, layer::Layer, service::Service};

use super::{
    Fallback, Route,
    method_router::MethodRouter,
    utils::{Matcher, NEST_CATCH_PARAM, RouteId, StripPrefixLayer},
};
use crate::{
    body::Body,
    context::ServerContext,
    request::Request,
    response::Response,
    server::{IntoResponse, handler::Handler},
};

/// The router for routing path to [`Service`]s or handlers.
#[must_use]
pub struct Router<B = Body, E = Infallible> {
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
    /// use volo_http::server::route::{Router, get};
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
    ///     route::{Router, get},
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
    ///     route::{Router, get},
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
    ///     route::{Router, get},
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
    ///     route::{Router, get},
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
    ///     route::{Router, get},
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
        S: Service<ServerContext, Request<B>, Error = E> + Send + Sync + 'static,
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
        for<'a> S: Service<ServerContext, Request<B>, Error = E> + Send + Sync + 'a,
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
    /// use volo_http::server::route::{Router, get};
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

        for (path, route_id) in matcher.drain() {
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
        L::Service: Service<ServerContext, Request<B2>, Error = E2> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, Request<B2>>>::Response: IntoResponse,
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

impl<B, E> Service<ServerContext, Request<B>> for Router<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    type Response = Response;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        if let Ok(matched) = self.matcher.at(req.uri().clone().path()) {
            if let Some(route) = self.routes.get(matched.value) {
                if !matched.params.is_empty() {
                    cx.params_mut().extend(matched.params);
                }
                return route.call(cx, req).await;
            }
        }

        self.fallback.call(cx, req).await
    }
}

#[allow(clippy::large_enum_variant)]
enum Endpoint<B = Body, E = Infallible> {
    MethodRouter(MethodRouter<B, E>),
    Service(Route<B, E>),
}

impl<B, E> Service<ServerContext, Request<B>> for Endpoint<B, E>
where
    B: Send + 'static,
    E: 'static,
{
    type Response = Response;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
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
        L::Service: Service<ServerContext, Request<B2>, Error = E2> + Send + Sync,
        <L::Service as Service<ServerContext, Request<B2>>>::Response: IntoResponse,
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

#[cfg(test)]
mod router_tests {
    use faststr::FastStr;
    use http::{method::Method, status::StatusCode, uri::Uri};

    use super::Router;
    use crate::{
        body::{Body, BodyConversion},
        server::{
            Server, param::PathParamsVec, route::method_router::any, test_helpers::TestServer,
        },
    };

    async fn always_ok() {}
    async fn teapot() -> StatusCode {
        StatusCode::IM_A_TEAPOT
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
            "/catch/114/514/1919/810\n114/514/1919/810",
        );

        // We register it as `/test-2/`, so `/test-2` does not match, but `/test-2/` does match.
        assert!(get_res(&server, "/test-2").await.is_empty());
        assert_eq!(get_res(&server, "/test-2/").await, "/");
        assert_eq!(get_res(&server, "/test-2/id/114").await, "/id/114\n114");
        assert_eq!(
            get_res(&server, "/test-2/catch/114/514/1919/810").await,
            "/catch/114/514/1919/810\n114/514/1919/810",
        );

        // The first param can be kept.
        assert_eq!(get_res(&server, "/test-3/114").await, "/\n114");
        assert_eq!(get_res(&server, "/test-3/114/").await, "/\n114");
        assert_eq!(
            get_res(&server, "/test-3/114/id/514").await,
            "/id/514\n114\n514",
        );
        assert_eq!(
            get_res(&server, "/test-3/114/catch/514/1919/810").await,
            "/catch/514/1919/810\n114\n514/1919/810",
        );

        // It is also empty.
        assert!(get_res(&server, "/test-4/114").await.is_empty());
        assert_eq!(get_res(&server, "/test-4/114/").await, "/\n114");
        assert_eq!(
            get_res(&server, "/test-4/114/id/514").await,
            "/id/514\n114\n514",
        );
        assert_eq!(
            get_res(&server, "/test-4/114/catch/514/1919/810").await,
            "/catch/514/1919/810\n114\n514/1919/810",
        );
    }

    #[tokio::test]
    async fn deep_nest_router() {
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

        // rule: `/test-1/{catch1}/test-2/{catch2}/test-3/{nest-route}`
        let router: Router<Option<Body>> = Router::new().nest(
            "/test-1/{catch1}",
            Router::new().nest(
                "/test-2/{catch2}/",
                Router::new().nest(
                    "/test-3",
                    Router::new()
                        .route("/", any(uri_and_params))
                        .route("/id/{id}", any(uri_and_params))
                        .route("/catch/{*content}", any(uri_and_params)),
                ),
            ),
        );
        let server = Server::new(router).into_test_server();

        // catch1: 114
        // catch2: 514
        // inner-uri: /
        assert_eq!(
            get_res(&server, "/test-1/114/test-2/514/test-3/").await,
            "/\n114\n514",
        );
        // catch1: 114
        // catch2: 514
        // inner-uri: /id/1919
        // id: 1919
        assert_eq!(
            get_res(&server, "/test-1/114/test-2/514/test-3/id/1919").await,
            "/id/1919\n114\n514\n1919",
        );
        // catch1: 114
        // catch2: 514
        // inner-uri: /id/1919
        // id: 1919
        assert_eq!(
            get_res(&server, "/test-1/114/test-2/514/test-3/catch/1919/810").await,
            "/catch/1919/810\n114\n514\n1919/810",
        );
    }

    #[tokio::test]
    async fn nest_router_with_query() {
        async fn get_query(uri: Uri) -> Result<String, StatusCode> {
            if let Some(query) = uri.query() {
                Ok(query.to_owned())
            } else {
                Err(StatusCode::BAD_REQUEST)
            }
        }
        async fn get_res(
            server: &TestServer<Router<Option<Body>>, Option<Body>>,
            uri: &str,
        ) -> Result<String, StatusCode> {
            let resp = server.call_route(Method::GET, uri, None).await;
            if resp.status().is_success() {
                Ok(resp
                    .into_string()
                    .await
                    .expect("response is not a valid string"))
            } else {
                Err(resp.status())
            }
        }

        let router: Router<Option<Body>> =
            Router::new().nest("/nest", Router::new().route("/query", any(get_query)));
        let server = Server::new(router).into_test_server();

        assert_eq!(
            get_res(&server, "/nest/query?foo=bar").await.unwrap(),
            "foo=bar",
        );
        assert_eq!(get_res(&server, "/nest/query?foo").await.unwrap(), "foo");
        assert_eq!(get_res(&server, "/nest/query?").await.unwrap(), "");
        assert!(get_res(&server, "/nest/query").await.is_err());
    }

    #[tokio::test]
    async fn deep_nest_router_with_query() {
        async fn get_query(uri: Uri) -> Result<String, StatusCode> {
            if let Some(query) = uri.query() {
                Ok(query.to_owned())
            } else {
                Err(StatusCode::BAD_REQUEST)
            }
        }
        async fn get_res(
            server: &TestServer<Router<Option<Body>>, Option<Body>>,
            uri: &str,
        ) -> Result<String, StatusCode> {
            let resp = server.call_route(Method::GET, uri, None).await;
            if resp.status().is_success() {
                Ok(resp
                    .into_string()
                    .await
                    .expect("response is not a valid string"))
            } else {
                Err(resp.status())
            }
        }

        let router: Router<Option<Body>> = Router::new().nest(
            "/nest-1",
            Router::new().nest(
                "/nest-2",
                Router::new().nest("/nest-3", Router::new().route("/query", any(get_query))),
            ),
        );
        let server = Server::new(router).into_test_server();

        assert_eq!(
            get_res(&server, "/nest-1/nest-2/nest-3/query?foo=bar")
                .await
                .unwrap(),
            "foo=bar",
        );
        assert_eq!(
            get_res(&server, "/nest-1/nest-2/nest-3/query?foo")
                .await
                .unwrap(),
            "foo",
        );
        assert_eq!(
            get_res(&server, "/nest-1/nest-2/nest-3/query?")
                .await
                .unwrap(),
            "",
        );
        assert!(
            get_res(&server, "/nest-1/nest-2/nest-3/query")
                .await
                .is_err()
        );
    }
}
