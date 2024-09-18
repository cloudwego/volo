//! Request builder for building a request and sending to server
//!
//! See [`RequestBuilder`] for more details.

use std::{error::Error, time::Duration};

use http::{
    header::{HeaderMap, HeaderName, HeaderValue},
    uri::PathAndQuery,
    Method, Request, Uri, Version,
};
use motore::service::Service;
use volo::net::Address;

use super::{callopt::CallOpt, target::Target, Client};
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
    target: Target,
    call_opt: Option<CallOpt>,
    request: ClientRequest<B>,
    timeout: Option<Duration>,
}

impl<'a, S> RequestBuilder<'a, S, Body> {
    pub(crate) fn new(client: &'a Client<S>) -> Self {
        Self {
            client,
            target: Default::default(),
            call_opt: Default::default(),
            request: Default::default(),
            timeout: None,
        }
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

    /// Set the request body as json from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "json")]
    pub fn json<T>(mut self, json: &T) -> Result<Self>
    where
        T: serde::Serialize,
    {
        let (mut parts, _) = self.request.into_parts();
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            mime::APPLICATION_JSON
                .essence_str()
                .parse()
                .expect("infallible"),
        );
        self.request = Request::from_parts(
            parts,
            crate::utils::json::serialize(json)
                .map_err(builder_error)?
                .into(),
        );

        Ok(self)
    }

    /// Set the request body as form from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "form")]
    pub fn form<T>(mut self, form: &T) -> Result<Self>
    where
        T: serde::Serialize,
    {
        let (mut parts, _) = self.request.into_parts();
        parts.headers.insert(
            http::header::CONTENT_TYPE,
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
        if let Some(target) = Target::from_uri(&uri) {
            let target = target?;
            self.target = target;
        }
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
        if let Some(target) = Target::from_uri(&uri) {
            let target = target?;
            self.target = target;
        }
        *self.request.uri_mut() = uri;
        Ok(self)
    }

    /// Set a [`CallOpt`] to the request.
    ///
    /// The [`CallOpt`] is used for service discover, default is an empty one.
    ///
    /// See [`CallOpt`] for more details.
    pub fn with_callopt(mut self, call_opt: CallOpt) -> Self {
        self.call_opt = Some(call_opt);
        self
    }

    /// Set query for the uri in request from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "query")]
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

    /// Get a reference to uri in the request.
    pub fn uri_ref(&self) -> &Uri {
        self.request.uri()
    }

    /// Set version of the HTTP request.
    pub fn version(mut self, version: Version) -> Self {
        *self.request.version_mut() = version;
        self
    }

    /// Get a reference to version in the request.
    pub fn version_ref(&self) -> Version {
        self.request.version()
    }

    /// Insert a header into the request header map.
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
        self.target = Target::from_address(address);
        self
    }

    /// Set target host for the request.
    ///
    /// It uses http with port 80 by default.
    ///
    /// For setting scheme and port, use [`Self::with_port`] and [`Self::with_https`] after
    /// specifying host.
    pub fn host<H>(mut self, host: H) -> Self
    where
        H: AsRef<str>,
    {
        self.target = Target::from_host(host);
        self
    }

    /// Set port for the target address of this request.
    pub fn with_port(mut self, port: u16) -> Self {
        self.target.set_port(port);
        self
    }

    /// Set if the request uses https.
    #[cfg(feature = "__tls")]
    pub fn with_https(mut self, https: bool) -> Self {
        self.target.set_https(https);
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

    /// Get a reference to [`CallOpt`].
    pub fn callopt_ref(&self) -> &Option<CallOpt> {
        &self.call_opt
    }

    /// Get a mutable reference to [`CallOpt`].
    pub fn callopt_mut(&mut self) -> &mut Option<CallOpt> {
        &mut self.call_opt
    }

    /// Set a request body.
    pub fn body<B2>(self, body: B2) -> RequestBuilder<'a, S, B2> {
        let (parts, _) = self.request.into_parts();
        let request = Request::from_parts(parts, body);

        RequestBuilder {
            client: self.client,
            target: self.target,
            call_opt: self.call_opt,
            request,
            timeout: self.timeout,
        }
    }

    /// Get a reference to body in the request.
    pub fn body_ref(&self) -> &B {
        self.request.body()
    }

    /// Set maximin idle time for the request.
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
            .send_request(self.target, self.call_opt, self.request, self.timeout)
            .await
    }
}

// The `httpbin.org` always responses a json data.
#[cfg(feature = "json")]
#[cfg(test)]
mod request_tests {
    #![allow(unused)]

    use std::collections::HashMap;

    use serde::Deserialize;

    use super::Client;
    use crate::body::BodyConversion;

    #[allow(dead_code)]
    #[derive(Deserialize)]
    struct HttpBinResponse {
        args: HashMap<String, String>,
        headers: HashMap<String, String>,
        origin: String,
        url: String,
    }

    #[cfg(feature = "query")]
    #[tokio::test]
    async fn set_query() {
        let mut builder = Client::builder();
        builder.host("httpbin.org");
        let client = builder.build();
        let query = HashMap::from([
            ("key".to_string(), "val".to_string()),
            ("key2".to_string(), "val2".to_string()),
        ]);
        let resp = client
            .get("/get")
            .unwrap()
            .set_query(&query)
            .unwrap()
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert_eq!(resp.args, query);
    }
}
