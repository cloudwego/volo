//! [`MethodRouter`] implementation for [`Server`].
//!
//! [`Router`] will route a path to a [`MethodRouter`], and the [`MethodRouter`] will route the
//! request through its HTTP method. If method of the request is not supported by the
//! [`MethodRouter`], it will fallback to another [`Route`].
//!
//! You can use a HTTP method name as a function for creating a [`MethodRouter`], for example,
//! [`get`] for creating a [`MethodRouter`] that can route a request with GET method to the target
//! [`Route`].
//!
//! See [`MethodRouter`] and [`get`], [`post`], [`any`], [`get_service`]... for more details.
//!
//! [`Server`]: crate::server::Server
//! [`Router`]: super::router::Router

use std::convert::Infallible;

use http::{method::Method, status::StatusCode};
use motore::{layer::Layer, service::Service, ServiceExt};
use paste::paste;

use super::{Fallback, Route};
use crate::{
    body::Body,
    context::ServerContext,
    request::Request,
    response::Response,
    server::{handler::Handler, IntoResponse},
};

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
///     request::Request,
///     server::route::{any, get, post_service, MethodRouter, Router},
/// };
///
/// async fn index() -> &'static str {
///     "Hello, World"
/// }
///
/// async fn index_fn(cx: &mut ServerContext, req: Request) -> Result<&'static str, Infallible> {
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
pub struct MethodRouter<B = Body, E = Infallible> {
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

impl<B, E> Service<ServerContext, Request<B>> for MethodRouter<B, E>
where
    B: Send,
{
    type Response = Response;
    type Error = E;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
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
        L::Service: Service<ServerContext, Request<B2>, Error = E2> + Send + Sync + 'static,
        <L::Service as Service<ServerContext, Request<B2>>>::Response: IntoResponse,
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
            for<'a> S: Service<ServerContext, Request<B>, Error = E>
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
        for<'a> S: Service<ServerContext, Request<B>, Error = E> + Send + Sync + 'a,
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
            for<'a> S: Service<ServerContext, Request<B>, Error = E>
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
    for<'a> S: Service<ServerContext, Request<B>, Error = E> + Send + Sync + 'a,
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
enum MethodEndpoint<B = Body, E = Infallible> {
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
        for<'a> S: Service<ServerContext, Request<B>, Error = E> + Send + Sync + 'a,
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

#[cfg(test)]
mod method_router_tests {
    use http::{method::Method, status::StatusCode};

    use super::{any, get, head, options, MethodRouter};
    use crate::body::Body;

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
}
