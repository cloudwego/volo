use std::{future::Future, net::SocketAddr};

use http::{Method, Response, StatusCode};
use http_body_util::Full;
use hyper::{
<<<<<<< HEAD
<<<<<<< HEAD
    body::{Body, Bytes, Incoming},
    server::conn::http1,
};
use motore::layer::Layer;
=======
    body::{Bytes, Incoming},
    server::conn::http1,
};
use hyper_util::rt::TokioIo;
>>>>>>> init
=======
    body::{Body, Bytes, Incoming},
    server::conn::http1,
};
use hyper_util::rt::TokioIo;
use motore::layer::Layer;
>>>>>>> layer (#224)
use tokio::net::TcpListener;

use crate::{
    dispatch::DispatchService, request::FromRequest, response::RespBody, DynError, HttpContext,
    MotoreService,
};

pub type DynService = motore::BoxCloneService<HttpContext, Incoming, Response<RespBody>, DynError>;

<<<<<<< HEAD
<<<<<<< HEAD
#[derive(Clone, Default)]
=======
#[derive(Clone)]
>>>>>>> init
=======
#[derive(Clone, Default)]
>>>>>>> layer (#224)
pub struct Router {
    inner: matchit::Router<DynService>,
}

<<<<<<< HEAD
<<<<<<< HEAD
<<<<<<< HEAD
=======
impl Router {
    pub fn build() -> RouterBuilder {
        Default::default()
    }
}

>>>>>>> init
=======
>>>>>>> layer (#224)
impl motore::Service<(), (HttpContextInner, Incoming)> for Router {
=======
impl motore::Service<HttpContext, Incoming> for Router {
>>>>>>> downgrade hyper version
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

<<<<<<< HEAD
<<<<<<< HEAD
impl Router {
=======
#[derive(Default)]
pub struct RouterBuilder {
    routes: matchit::Router<DynService>,
}

impl RouterBuilder {
>>>>>>> init
=======
impl Router {
>>>>>>> layer (#224)
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
<<<<<<< HEAD
<<<<<<< HEAD
        if let Err(e) = self.inner.insert(uri, motore::BoxCloneService::new(route)) {
=======
        if let Err(e) = self.routes.insert(uri, motore::BoxCloneService::new(route)) {
>>>>>>> init
=======
        if let Err(e) = self.inner.insert(uri, motore::BoxCloneService::new(route)) {
>>>>>>> layer (#224)
            panic!("routing error: {e}");
        }
        self
    }
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> layer (#224)
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
<<<<<<< HEAD

#[async_trait::async_trait]
pub trait Server {
    async fn serve(self, addr: SocketAddr) -> Result<(), DynError>;
}
#[async_trait::async_trait]
impl<S, OB> Server for S
where
    S: motore::Service<(), (HttpContextInner, Incoming), Response = Response<OB>>
        + Clone
        + Send
        + Sync
        + 'static,
    OB: Body<Error = DynError> + Send + 'static,
    <OB as Body>::Data: Send,
    <S as motore::Service<(), (HttpContextInner, Incoming)>>::Error: Into<DynError>,
{
    async fn serve(self, addr: SocketAddr) -> Result<(), DynError> {
        let listener = TcpListener::bind(addr).await?;

        let service = self;
        loop {
            let s = service.clone();
=======
=======
>>>>>>> layer (#224)

#[async_trait::async_trait]
pub trait Server {
    async fn serve(self, addr: SocketAddr) -> Result<(), DynError>;
}
#[async_trait::async_trait]
impl<S, OB> Server for S
where
    S: motore::Service<HttpContext, Incoming, Response = Response<OB>>
        + Clone
        + Send
        + Sync
        + 'static,
    OB: Body<Error = DynError> + Send + 'static,
    <OB as Body>::Data: Send,
    <S as motore::Service<HttpContext, Incoming>>::Error: Into<DynError>,
{
    async fn serve(self, addr: SocketAddr) -> Result<(), DynError> {
        let listener = TcpListener::bind(addr).await?;

        let service = self;
        loop {
<<<<<<< HEAD
            let s = router.clone();
>>>>>>> init
=======
            let s = service.clone();
>>>>>>> layer (#224)
            let (stream, peer) = listener.accept().await?;

            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(
                        stream,
                        MotoreService {
                            peer: peer.into(),
                            inner: s,
                        },
                    )
                    .await
                {
                    tracing::warn!("error serving connection: {:?}", err);
                }
            });
        }
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
