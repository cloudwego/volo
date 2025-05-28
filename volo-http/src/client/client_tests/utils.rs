use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use motore::{layer::Layer, service::Service};

use crate::{body::Body, request::Request, response::Response};

pub struct RespBodyToFullLayer;
pub struct RespBodyToFullService<S>(S);

impl<S> Layer<S> for RespBodyToFullLayer {
    type Service = RespBodyToFullService<S>;

    fn layer(self, inner: S) -> Self::Service {
        RespBodyToFullService(inner)
    }
}

impl<Cx, ReqBody, RespBody, S> Service<Cx, Request<ReqBody>> for RespBodyToFullService<S>
where
    Cx: Send,
    S: Service<Cx, Request<ReqBody>, Response = Response<RespBody>> + Sync,
    ReqBody: Send,
    RespBody: http_body::Body + Send,
    RespBody::Data: Send,
    RespBody::Error: std::fmt::Debug + Send,
{
    type Response = Response<Full<Bytes>>;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut Cx,
        req: Request<ReqBody>,
    ) -> Result<Self::Response, Self::Error> {
        let resp = self.0.call(cx, req).await?;
        let (parts, body) = resp.into_parts();
        let body = Full::new(BodyExt::collect(body).await.unwrap().to_bytes());
        let resp = Response::from_parts(parts, body);
        Ok(resp)
    }
}

// For request, and for testing, it should never implement `http_body::Body`
pub struct AutoBody;

pub struct AutoBodyLayer;
pub struct AutoBodyService<S>(S);

impl<S> Layer<S> for AutoBodyLayer {
    type Service = AutoBodyService<S>;

    fn layer(self, inner: S) -> Self::Service {
        AutoBodyService(inner)
    }
}

impl<Cx, RespBody, S> Service<Cx, Request<AutoBody>> for AutoBodyService<S>
where
    Cx: Send,
    S: Service<Cx, Request, Response = Response<RespBody>> + Sync,
    RespBody: Send,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut Cx,
        req: Request<AutoBody>,
    ) -> Result<Self::Response, Self::Error> {
        let (parts, _) = req.into_parts();
        let body = Body::from("Hello, World");
        let req = Request::from_parts(parts, body);
        self.0.call(cx, req).await
    }
}

// For request, and for testing, it should never implement `http_body::Body`
pub struct AutoFull;

pub struct AutoFullLayer;
pub struct AutoFullService<S>(S);

impl<S> Layer<S> for AutoFullLayer {
    type Service = AutoFullService<S>;

    fn layer(self, inner: S) -> Self::Service {
        AutoFullService(inner)
    }
}

impl<Cx, RespBody, S> Service<Cx, Request<AutoFull>> for AutoFullService<S>
where
    Cx: Send,
    S: Service<Cx, Request<Full<Bytes>>, Response = Response<RespBody>> + Sync,
    RespBody: Send,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut Cx,
        req: Request<AutoFull>,
    ) -> Result<Self::Response, Self::Error> {
        let (parts, _) = req.into_parts();
        let body = Full::new(Bytes::new());
        let req = Request::from_parts(parts, body);
        self.0.call(cx, req).await
    }
}

// For response, and for testing, it should never implement `http_body::Body`
pub struct Nothing;

pub struct DropBodyLayer;
pub struct DropBodyService<S>(S);

impl<S> Layer<S> for DropBodyLayer {
    type Service = DropBodyService<S>;

    fn layer(self, inner: S) -> Self::Service {
        DropBodyService(inner)
    }
}

impl<Cx, ReqBody, RespBody, S> Service<Cx, Request<ReqBody>> for DropBodyService<S>
where
    Cx: Send,
    S: Service<Cx, Request<ReqBody>, Response = Response<RespBody>> + Sync,
    ReqBody: Send,
    RespBody: Send,
{
    type Response = Response<Nothing>;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut Cx,
        req: Request<ReqBody>,
    ) -> Result<Self::Response, Self::Error> {
        let resp = self.0.call(cx, req).await?;
        let (parts, _) = resp.into_parts();
        let body = Nothing;
        let resp = Response::from_parts(parts, body);
        Ok(resp)
    }
}
