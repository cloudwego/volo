#![deny(missing_docs)]

use std::{error::Error, time::Duration};

use faststr::FastStr;
use http::{
    header,
    header::{HeaderMap, HeaderName, HeaderValue},
    uri::{PathAndQuery, Scheme},
    Method, Request, Uri, Version,
};
use motore::service::Service;
use volo::net::Address;

use super::{utils::TargetBuilder, Client};
use crate::{
    body::Body,
    context::ClientContext,
    error::{
        client::{builder_error, Result},
        BoxError, ClientError,
    },
    request::ClientRequest,
    response::ClientResponse,
};

/// The builder for building a request.
pub struct RequestBuilder<'a, S, B = Body> {
    client: &'a Client<S>,
    target: TargetBuilder,
    request: ClientRequest<B>,
    timeout: Option<Duration>,
}

impl<'a, S> RequestBuilder<'a, S, Body> {
    pub(crate) fn new(client: &'a Client<S>) -> Self {
        Self {
            client,
            target: TargetBuilder::None,
            request: Request::new(Body::empty()),
            timeout: None,
        }
    }

    pub(crate) fn new_with_method_and_uri(
        client: &'a Client<S>,
        method: Method,
        uri: Uri,
    ) -> Result<Self> {
        let rela_uri = uri
            .path_and_query()
            .map(PathAndQuery::as_str)
            .unwrap_or("/")
            .to_owned();

        let mut builder = Self {
            client,
            target: TargetBuilder::None,
            request: Request::builder()
                .method(method)
                .uri(rela_uri)
                .body(Body::empty())
                .map_err(builder_error)?,
            timeout: None,
        };
        builder.fill_target(&uri);

        Ok(builder)
    }

    /// Set the request body.
    pub fn data<D>(mut self, data: D) -> Result<Self>
    where
        D: TryInto<Body>,
        D::Error: Error + Send + Sync + 'static,
    {
        let (parts, _) = self.request.into_parts();
        self.request = Request::from_parts(parts, data.try_into().map_err(builder_error)?);

        Ok(self)
    }

    /// Set the request body as json from object with `Serialize`.
    #[cfg(feature = "__json")]
    #[cfg_attr(docsrs, doc(cfg(feature = "json")))]
    pub fn json<T>(mut self, json: &T) -> Result<Self>
    where
        T: serde::Serialize,
    {
        let (mut parts, _) = self.request.into_parts();
        parts.headers.insert(
            header::CONTENT_TYPE,
            mime::APPLICATION_JSON
                .essence_str()
                .parse()
                .expect("infallible"),
        );
        self.request = Request::from_parts(
            parts,
            crate::json::serialize(json).map_err(builder_error)?.into(),
        );

        Ok(self)
    }

    /// Set the request body as form from object with `Serialize`.
    #[cfg(feature = "form")]
    #[cfg_attr(docsrs, doc(cfg(feature = "form")))]
    pub fn form<T>(mut self, form: &T) -> Result<Self>
    where
        T: serde::Serialize,
    {
        let (mut parts, _) = self.request.into_parts();
        parts.headers.insert(
            header::CONTENT_TYPE,
            mime::APPLICATION_WWW_FORM_URLENCODED
                .essence_str()
                .parse()
                .expect("infallible"),
        );
        self.request = Request::from_parts(
            parts,
            serde_urlencoded::to_string(form)
                .map_err(builder_error)?
                .into(),
        );

        Ok(self)
    }
}

impl<'a, S, B> RequestBuilder<'a, S, B> {
    /// Set method for the request.
    pub fn method(mut self, method: Method) -> Self {
        *self.request.method_mut() = method;
        self
    }

    /// Get the reference of method in the request.
    pub fn method_ref(&self) -> &Method {
        self.request.method()
    }

    fn fill_target(&mut self, uri: &Uri) {
        if let Some(host) = uri.host() {
            self.target = TargetBuilder::Host {
                scheme: uri.scheme().cloned(),
                host: FastStr::from_string(host.to_owned()),
                port: uri.port_u16(),
            };
        }
    }

    /// Set uri for building request.
    ///
    /// The uri will be split into two parts scheme+host and path+query. The scheme and host can be
    /// empty and it will be resolved as the target address. The path and query must exist and they
    /// are used to build the request uri.
    ///
    /// Note that only path and query will be set to the request uri. For setting the full uri, use
    /// `full_uri` instead.
    pub fn uri<U>(mut self, uri: U) -> Result<Self>
    where
        U: TryInto<Uri>,
        U::Error: Into<BoxError>,
    {
        let uri = uri.try_into().map_err(builder_error)?;
        let rela_uri = uri
            .path_and_query()
            .map(PathAndQuery::to_owned)
            .unwrap_or_else(|| PathAndQuery::from_static("/"))
            .into();
        self.fill_target(&uri);
        *self.request.uri_mut() = rela_uri;
        Ok(self)
    }

    /// Set full uri for building request.
    ///
    /// In this function, scheme and host will be resolved as the target address, and the full uri
    /// will be set as the request uri.
    ///
    /// This function is only used for using http(s) proxy.
    pub fn full_uri<U>(mut self, uri: U) -> Result<Self>
    where
        U: TryInto<Uri>,
        U::Error: Into<BoxError>,
    {
        let uri = uri.try_into().map_err(builder_error)?;
        self.fill_target(&uri);
        *self.request.uri_mut() = uri;
        Ok(self)
    }

    /// Set query for the uri in request from object with `Serialize`.
    #[cfg(feature = "query")]
    #[cfg_attr(docsrs, doc(cfg(feature = "query")))]
    pub fn set_query<T>(mut self, query: &T) -> Result<Self>
    where
        T: serde::Serialize,
    {
        let mut path = self.request.uri().path().to_owned();
        path.push('?');
        let query_str = serde_urlencoded::to_string(query).map_err(builder_error)?;
        path.push_str(&query_str);

        *self.request.uri_mut() = Uri::from_maybe_shared(path).map_err(builder_error)?;

        Ok(self)
    }

    /// Get the reference of uri in the request.
    pub fn uri_ref(&self) -> &Uri {
        self.request.uri()
    }

    /// Set the version of the HTTP request.
    pub fn version(mut self, version: Version) -> Self {
        *self.request.version_mut() = version;
        self
    }

    /// Get the reference of version in the request.
    pub fn version_ref(&self) -> Version {
        self.request.version()
    }

    /// Insert a header into the request.
    pub fn header<K, V>(mut self, key: K, value: V) -> Result<Self>
    where
        K: TryInto<HeaderName>,
        K::Error: Into<http::Error>,
        V: TryInto<HeaderValue>,
        V::Error: Into<http::Error>,
    {
        self.request.headers_mut().insert(
            key.try_into().map_err(|e| builder_error(e.into()))?,
            value.try_into().map_err(|e| builder_error(e.into()))?,
        );
        Ok(self)
    }

    /// Get the reference of headers in the request.
    pub fn headers(&self) -> &HeaderMap {
        self.request.headers()
    }

    /// Get the mutable reference of headers in the request.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.request.headers_mut()
    }

    /// Set the target address for the request.
    pub fn address<A>(
        mut self,
        address: A,
        #[cfg(feature = "__tls")]
        #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
        use_tls: bool,
    ) -> Self
    where
        A: Into<Address>,
    {
        self.target = TargetBuilder::Address {
            addr: address.into(),
            #[cfg(feature = "__tls")]
            use_tls,
        };
        self
    }

    /// Set the target host for the request.
    ///
    /// If TLS is enabled, it will use https with port 443 by default, otherwise it will use http
    /// with port 80 by default.
    ///
    /// For setting the scheme and port, use `scheme_host_and_port` instead.
    pub fn host<IS>(mut self, host: IS) -> Self
    where
        IS: Into<FastStr>,
    {
        self.target = TargetBuilder::Host {
            scheme: None,
            host: host.into(),
            port: None,
        };
        self
    }

    /// Set the target scheme, host and port for the request.
    pub fn scheme_host_and_port<IS>(
        mut self,
        scheme: Option<Scheme>,
        host: IS,
        port: Option<u16>,
    ) -> Self
    where
        IS: Into<FastStr>,
    {
        self.target = TargetBuilder::Host {
            scheme,
            host: host.into(),
            port,
        };
        self
    }

    /// Set the request body.
    pub fn body<B2>(self, body: B2) -> RequestBuilder<'a, S, B2> {
        let (parts, _) = self.request.into_parts();
        let request = Request::from_parts(parts, body);

        RequestBuilder {
            client: self.client,
            target: self.target,
            request,
            timeout: self.timeout,
        }
    }

    /// Get the reference of body in the request.
    pub fn body_ref(&self) -> &B {
        self.request.body()
    }

    /// Set the maximin idle time for the request.
    ///
    /// The whole request includes connecting, writting, and reading the whole HTTP protocol
    /// headers (without reading response body).
    pub fn set_request_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

impl<'a, S, B> RequestBuilder<'a, S, B>
where
    S: Service<ClientContext, ClientRequest<B>, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync
        + 'static,
    B: Send + 'static,
{
    /// Send the request and get the response.
    pub async fn send(self) -> Result<ClientResponse> {
        self.client
            .send_request(self.target, self.request, self.timeout)
            .await
    }
}
