//! Cookie implementation
//!
//! This module provides [`CookieLayer`] for extracting and setting cookies.

use motore::{layer::Layer, Service};
use parking_lot::RwLock;

use crate::{
    context::ClientContext,
    error::ClientError,
    request::{ClientRequest, RequestPartsExt},
    response::ClientResponse,
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

        if let Some(url) = &url {
            let (mut parts, body) = req.into_parts();
            if parts.headers.get(http::header::COOKIE).is_none() {
                self.cookie_store
                    .read()
                    .add_cookie_header(&mut parts.headers, url);
            }
            req = ClientRequest::from_parts(parts, body);
        }

        let resp = self.inner.call(cx, req).await?;

        if let Some(url) = &url {
            self.cookie_store
                .write()
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
    /// Create a new [`CookieLayer`] with the given [`CookieStore`].
    ///
    /// It will set cookies from the [`CookieStore`] into the request header before sending the
    /// request,
    ///
    /// and store cookies after receiving the response.
    ///
    /// It is recommended to use [`CookieLayer`] as the innermost layer in the client stack,
    ///
    /// since it will extract cookies from the request header and store them in the [`CookieStore`].
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::{client::cookie::CookieLayer, Client};
    ///
    /// let builder = Client::builder();
    /// let client = builder
    ///     .layer_inner(CookieLayer::new(Default::default()))
    ///     .build();
    /// ```
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
