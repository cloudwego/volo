//! Cookie implementation
//!
//! This module provides [`CookieLayer`] for extracting and setting cookies.
//!
//! See [`CookieLayer`] for more details.

use motore::{layer::Layer, Service};
use tokio::sync::RwLock;

use crate::{
    context::ClientContext,
    error::ClientError,
    request::{Request, RequestPartsExt},
    response::Response,
    utils::cookie::CookieStore,
};

/// [`CookieLayer`] generated [`Service`]
///
/// See [`CookieLayer`] for more details.
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

impl<S, ReqBody, RespBody> Service<ClientContext, Request<ReqBody>> for CookieService<S>
where
    S: Service<ClientContext, Request<ReqBody>, Response = Response<RespBody>, Error = ClientError>
        + Send
        + Sync
        + 'static,
    ReqBody: Send,
    RespBody: Send,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut req: Request<ReqBody>,
    ) -> Result<Self::Response, Self::Error> {
        let url = req.url();

        if let Some(url) = &url {
            let (mut parts, body) = req.into_parts();
            if parts.headers.get(http::header::COOKIE).is_none() {
                self.cookie_store
                    .read()
                    .await
                    .add_cookie_header(&mut parts.headers, url);
            }
            req = Request::from_parts(parts, body);
        }

        let resp = self.inner.call(cx, req).await?;

        if let Some(url) = &url {
            self.cookie_store
                .write()
                .await
                .store_response_headers(resp.headers(), url);
        }

        Ok(resp)
    }
}

/// [`Layer`] for extracting and setting cookies.
///
/// See [`CookieLayer::new`] for more details.
pub struct CookieLayer {
    cookie_store: RwLock<CookieStore>,
}

impl CookieLayer {
    /// Create a new [`CookieLayer`] with the given [` CookieStore`](cookie_store::CookieStore).
    ///
    /// It will set cookies from the [`CookieStore`](cookie_store::CookieStore) into the request
    /// header before sending the request,
    ///
    /// and store cookies after receiving the response.
    ///
    /// It is recommended to use [`CookieLayer`] as the innermost layer in the client stack
    /// since it will extract cookies from the request header and store them before and after call
    /// the transport layer.
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::{client::cookie::CookieLayer, Client};
    ///
    /// let client: Client = Client::builder()
    ///     .layer_inner(CookieLayer::new(Default::default()))
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn new(cookie_store: cookie_store::CookieStore) -> Self {
        Self {
            cookie_store: RwLock::new(CookieStore::new(cookie_store)),
        }
    }
}

impl<S> Layer<S> for CookieLayer {
    type Service = CookieService<S>;

    fn layer(self, inner: S) -> Self::Service {
        CookieService::new(inner, self.cookie_store)
    }
}
