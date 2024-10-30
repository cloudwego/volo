use std::sync::RwLock;

use motore::{layer::Layer, Service};

use crate::{
    context::ClientContext,
    error::ClientError,
    request::{ClientRequest, RequestPartsExt},
    response::ClientResponse,
    utils::cookie::CookieStore,
};

pub struct CookieService<S> {
    inner: S,
    cookie_store: RwLock<CookieStore>,
}

impl<S> CookieService<S> {
    fn new(inner: S, cookie_store: RwLock<CookieStore>) -> Self {
        Self {
            inner,
            cookie_store,
        }
    }
}

impl<S, B> Service<ClientContext, ClientRequest<B>> for CookieService<S>
where
    S: Service<ClientContext, ClientRequest<B>, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync
        + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut req: ClientRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        let url = req.url();

        let (mut parts, body) = req.into_parts();
        if let Some(url) = &url {
            if parts.headers.get(http::header::COOKIE).is_none() {
                self.cookie_store
                    .read()
                    .unwrap()
                    .add_cookie_header(&mut parts.headers, url);
            }
        }

        req = ClientRequest::from_parts(parts, body);

        let resp = self.inner.call(cx, req).await?;

        if let Some(url) = &url {
            self.cookie_store
                .write()
                .unwrap()
                .with_response_headers(resp.headers(), url);
        }

        Ok(resp)
    }
}

pub struct CookieLayer {
    cookie_store: RwLock<CookieStore>,
}

impl CookieLayer {
    pub fn new(cookie_store: CookieStore) -> Self {
        Self {
            cookie_store: RwLock::new(cookie_store),
        }
    }
}

impl<S> Layer<S> for CookieLayer {
    type Service = CookieService<S>;

    fn layer(self, inner: S) -> Self::Service {
        CookieService::new(inner, self.cookie_store)
    }
}
