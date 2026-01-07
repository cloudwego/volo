//! Request builder for building a request and sending to server
//!
//! See [`RequestBuilder`] for more details.

use std::{borrow::Cow, error::Error};

use faststr::FastStr;
use http::{
    header::{HeaderMap, HeaderName, HeaderValue},
    method::Method,
    uri::{PathAndQuery, Scheme, Uri},
    version::Version,
};
use motore::layer::Layer;
use volo::{
    client::{Apply, OneShotService, WithOptService},
    net::Address,
};

use super::{CallOpt, insert_header, target::Target};
use crate::{
    body::Body,
    context::ClientContext,
    error::{
        BoxError, ClientError,
        client::{Result, builder_error},
    },
    request::Request,
    response::Response,
    utils::consts,
};

/// The builder for building a request.
pub struct RequestBuilder<S, B = Body> {
    inner: S,
    target: Target,
    version: Option<Version>,
    request: Request<B>,
    status: Result<()>,
}

impl<S> RequestBuilder<S> {
    pub(super) fn new(inner: S) -> Self {
        Self {
            inner,
            target: Default::default(),
            version: None,
            request: Request::default(),
            status: Ok(()),
        }
    }

    /// Set the request body.
    pub fn data<D>(mut self, data: D) -> Self
    where
        D: TryInto<Body>,
        D::Error: Error + Send + Sync + 'static,
    {
        if self.status.is_err() {
            return self;
        }

        let body = match data.try_into() {
            Ok(body) => body,
            Err(err) => {
                self.status = Err(builder_error(err));
                return self;
            }
        };

        let (parts, _) = self.request.into_parts();
        self.request = Request::from_parts(parts, body);

        self
    }

    /// Set the request body as json from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "json")]
    pub fn json<T>(mut self, json: &T) -> Self
    where
        T: serde::Serialize,
    {
        if self.status.is_err() {
            return self;
        }

        let json = match crate::utils::json::serialize(json) {
            Ok(json) => json,
            Err(err) => {
                self.status = Err(builder_error(err));
                return self;
            }
        };

        let (mut parts, _) = self.request.into_parts();
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            crate::utils::consts::APPLICATION_JSON,
        );
        self.request = Request::from_parts(parts, Body::from(json));

        self
    }

    /// Set the request body as form from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "form")]
    pub fn form<T>(mut self, form: &T) -> Self
    where
        T: serde::Serialize,
    {
        if self.status.is_err() {
            return self;
        }

        let form = match serde_urlencoded::to_string(form) {
            Ok(form) => form,
            Err(err) => {
                self.status = Err(builder_error(err));
                return self;
            }
        };

        let (mut parts, _) = self.request.into_parts();
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            crate::utils::consts::APPLICATION_WWW_FORM_URLENCODED,
        );
        self.request = Request::from_parts(parts, Body::from(form));

        self
    }
}

impl<S, B> RequestBuilder<S, B> {
    /// Set method for the request.
    pub fn method(mut self, method: Method) -> Self {
        *self.request.method_mut() = method;
        self
    }

    /// Get a reference to method in the request.
    pub fn method_ref(&self) -> &Method {
        self.request.method()
    }

    /// Set uri for building request.
    ///
    /// The uri will be split into two parts scheme+host and path+query. The scheme and host can be
    /// empty and it will be resolved as the target address. The path and query must exist and they
    /// are used to build the request uri.
    ///
    /// Note that only path and query will be set to the request uri. For setting the full uri, use
    /// `full_uri` instead.
    pub fn uri<U>(mut self, uri: U) -> Self
    where
        U: TryInto<Uri>,
        U::Error: Into<BoxError>,
    {
        if self.status.is_err() {
            return self;
        }
        let uri = match uri.try_into() {
            Ok(uri) => uri,
            Err(err) => {
                self.status = Err(builder_error(err));
                return self;
            }
        };
        if uri.host().is_some() {
            match Target::from_uri(&uri) {
                Ok(target) => self.target = target,
                Err(err) => {
                    self.status = Err(err);
                    return self;
                }
            }
        }
        let rela_uri = uri
            .path_and_query()
            .map(PathAndQuery::to_owned)
            .unwrap_or_else(|| PathAndQuery::from_static("/"))
            .into();
        *self.request.uri_mut() = rela_uri;

        self
    }

    /// Set query for the uri in request from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "query")]
    pub fn set_query<T>(mut self, query: &T) -> Self
    where
        T: serde::Serialize,
    {
        if self.status.is_err() {
            return self;
        }
        let query_str = match serde_urlencoded::to_string(query) {
            Ok(query) => query,
            Err(err) => {
                self.status = Err(builder_error(err));
                return self;
            }
        };

        // We should keep path only without query
        let path_str = self.request.uri().path();
        let mut path = String::with_capacity(path_str.len() + 1 + query_str.len());
        path.push_str(path_str);
        path.push('?');
        path.push_str(&query_str);
        let Ok(uri) = Uri::from_maybe_shared(path) else {
            // path part is from a valid uri, and the result of urlencoded must be valid.
            unreachable!();
        };

        *self.request.uri_mut() = uri;

        self
    }

    /// Get a reference to uri in the request.
    pub fn uri_ref(&self) -> &Uri {
        self.request.uri()
    }

    /// Set version of the HTTP request.
    ///
    /// If it is not set, the request will use HTTP/2 if it is enabled and supported by default.
    pub fn version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Get a reference to version in the request.
    pub fn version_ref(&self) -> Option<Version> {
        self.version
    }

    /// Insert a header into the request header map.
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        K: TryInto<HeaderName>,
        K::Error: Error + Send + Sync + 'static,
        V: TryInto<HeaderValue>,
        V::Error: Error + Send + Sync + 'static,
    {
        if self.status.is_err() {
            return self;
        }

        if let Err(err) = insert_header(self.request.headers_mut(), key, value) {
            self.status = Err(err);
        }

        self
    }

    /// Get a reference to headers in the request.
    pub fn headers(&self) -> &HeaderMap {
        self.request.headers()
    }

    /// Get a mutable reference to headers in the request.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.request.headers_mut()
    }

    /// Set target address for the request.
    pub fn address<A>(mut self, address: A) -> Self
    where
        A: Into<Address>,
    {
        self.target = Target::from(address.into());
        self
    }

    /// Set target host for the request.
    ///
    /// It uses http with port 80 by default.
    ///
    /// For setting scheme and port, use [`Self::with_scheme`] and [`Self::with_port`] after
    /// specifying host.
    pub fn host<H>(mut self, host: H) -> Self
    where
        H: Into<Cow<'static, str>>,
    {
        // SAFETY: using HTTP is safe
        self.target = unsafe {
            Target::new_host_unchecked(
                Scheme::HTTP,
                FastStr::from(host.into()),
                consts::HTTP_DEFAULT_PORT,
            )
        };
        self
    }

    /// Set scheme for target of the request.
    pub fn with_scheme(mut self, scheme: Scheme) -> Self {
        if self.status.is_err() {
            return self;
        }
        if let Err(err) = self.target.set_scheme(scheme) {
            self.status = Err(err);
        }
        self
    }

    /// Set port for target address of this request.
    pub fn with_port(mut self, port: u16) -> Self {
        if self.status.is_err() {
            return self;
        }
        if let Err(err) = self.target.set_port(port) {
            self.status = Err(err);
        }
        self
    }

    /// Get a reference to [`Target`].
    pub fn target_ref(&self) -> &Target {
        &self.target
    }

    /// Get a mutable reference to [`Target`].
    pub fn target_mut(&mut self) -> &mut Target {
        &mut self.target
    }

    /// Set a request body.
    pub fn body<B2>(self, body: B2) -> RequestBuilder<S, B2> {
        let (parts, _) = self.request.into_parts();
        let request = Request::from_parts(parts, body);

        RequestBuilder {
            inner: self.inner,
            target: self.target,
            version: self.version,
            request,
            status: self.status,
        }
    }

    /// Get a reference to body in the request.
    pub fn body_ref(&self) -> &B {
        self.request.body()
    }

    /// Add a new [`Layer`] to the front of request builder.
    ///
    /// Note that the [`Layer`] generated `Service` should be a [`OneShotService`].
    pub fn layer<L>(self, layer: L) -> RequestBuilder<L::Service, B>
    where
        L: Layer<S>,
    {
        RequestBuilder {
            inner: layer.layer(self.inner),
            target: self.target,
            version: self.version,
            request: self.request,
            status: self.status,
        }
    }

    /// Apply a [`CallOpt`] to the request.
    pub fn with_callopt(self, callopt: CallOpt) -> RequestBuilder<WithOptService<S, CallOpt>, B> {
        self.layer(WithOptLayer::new(callopt))
    }

    fn set_version(&mut self) {
        let ver = match self.version {
            Some(ver) => ver,
            None => {
                // Use HTTP/1.1 by default
                if cfg!(feature = "http1") {
                    Version::HTTP_11
                } else {
                    Version::HTTP_2
                }
            }
        };
        *self.request.version_mut() = ver;
    }

    /// Send the request and get the response.
    pub async fn send<RespBody>(mut self) -> Result<Response<RespBody>>
    where
        S: OneShotService<
                ClientContext,
                Request<B>,
                Response = Response<RespBody>,
                Error = ClientError,
            > + Send
            + Sync
            + 'static,
        B: Send + 'static,
    {
        self.set_version();
        self.status?;

        let mut cx = ClientContext::new();
        self.target.apply(&mut cx)?;
        self.inner.call(&mut cx, self.request).await
    }
}

struct WithOptLayer {
    opt: CallOpt,
}

impl WithOptLayer {
    const fn new(opt: CallOpt) -> Self {
        Self { opt }
    }
}

impl<S> Layer<S> for WithOptLayer {
    type Service = WithOptService<S, CallOpt>;

    fn layer(self, inner: S) -> Self::Service {
        WithOptService::new(inner, self.opt)
    }
}
