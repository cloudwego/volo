use std::error::Error;

use http::{uri::PathAndQuery, HeaderMap, HeaderName, HeaderValue, Method, Request, Uri, Version};
use motore::service::Service;
use volo::net::Address;

use super::Client;
use crate::{
    body::Body,
    client::utils::{parse_address, resolve},
    context::ClientContext,
    error::{
        client::{bad_host_name, builder_error, no_uri, ClientErrorInner, Result},
        ClientError,
    },
    request::ClientRequest,
    response::ClientResponse,
};

pub struct RequestBuilder<'a, S, B = Body> {
    client: &'a Client<S>,
    target: Option<Address>,
    uri: Option<Uri>,
    request: ClientRequest<B>,
}

impl<'a, S> RequestBuilder<'a, S, Body> {
    pub(crate) fn new(client: &'a Client<S>) -> Self {
        Self {
            client,
            target: None,
            uri: None,
            request: Request::new(Body::empty()),
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

        Ok(Self {
            client,
            target: None,
            uri: Some(uri),
            request: Request::builder()
                .method(method)
                .uri(rela_uri)
                .body(Body::empty())
                .map_err(builder_error)?,
        })
    }

    pub fn data<D>(mut self, data: D) -> Result<Self>
    where
        D: TryInto<Body>,
        D::Error: Error + Send + Sync + 'static,
    {
        let (parts, _) = self.request.into_parts();
        self.request = Request::from_parts(parts, data.try_into().map_err(builder_error)?);

        Ok(self)
    }

    #[cfg(feature = "__json")]
    pub fn json<T>(mut self, json: &T) -> Result<Self>
    where
        T: serde::Serialize,
    {
        let (parts, _) = self.request.into_parts();
        self.request = Request::from_parts(
            parts,
            crate::json::serialize(json).map_err(builder_error)?.into(),
        );

        Ok(self)
    }
}

impl<'a, S, B> RequestBuilder<'a, S, B> {
    pub fn method(mut self, method: Method) -> Self {
        *self.request.method_mut() = method;
        self
    }

    pub fn method_ref(&self) -> &Method {
        self.request.method()
    }

    /// Set uri for building request.
    ///
    /// Note that the param `uri` must be a full uri, it will be checked and only relative uri
    /// (path and query) will be used in request.
    pub fn uri(mut self, uri: Uri) -> Result<Self> {
        let rela_uri = uri
            .path_and_query()
            .map(PathAndQuery::to_owned)
            .unwrap_or_else(|| PathAndQuery::from_static("/"))
            .into();
        self.uri = Some(uri);
        *self.request.uri_mut() = rela_uri;
        Ok(self)
    }

    /// Set full uri for building request.
    ///
    /// This function is only used for using http(s) proxy.
    pub fn absolute_uri(mut self, uri: Uri) -> Self {
        self.uri = Some(uri.clone());
        *self.request.uri_mut() = uri;
        self
    }

    pub fn uri_ref(&self) -> &Uri {
        self.request.uri()
    }

    pub fn version(mut self, version: Version) -> Self {
        *self.request.version_mut() = version;
        self
    }

    pub fn version_ref(&self) -> Version {
        self.request.version()
    }

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

    pub fn headers(&self) -> &HeaderMap {
        self.request.headers()
    }

    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.request.headers_mut()
    }

    pub fn target(mut self, target: Address) -> Self {
        self.target = Some(target);
        self
    }

    pub fn target_ref(&self) -> Option<&Address> {
        self.target.as_ref()
    }

    pub fn body<B2>(self, body: B2) -> RequestBuilder<'a, S, B2> {
        let (parts, _) = self.request.into_parts();
        let request = Request::from_parts(parts, body);

        RequestBuilder {
            client: self.client,
            target: self.target,
            uri: self.uri,
            request,
        }
    }

    pub fn body_ref(&self) -> &B {
        self.request.body()
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
    pub async fn send(self) -> Result<ClientResponse> {
        let uri = self.uri.ok_or_else(no_uri)?;
        let target = match self.target {
            Some(target) => target,
            None => match parse_address(&uri) {
                Ok(addr) => addr,
                Err(err) => {
                    if matches!(
                        err.inner(),
                        ClientErrorInner::UriWithoutHost | ClientErrorInner::BadScheme
                    ) {
                        return Err(err);
                    }
                    resolve(&uri).await?.next().ok_or_else(bad_host_name)?
                }
            },
        };
        self.client.send_request(uri, target, self.request).await
    }
}
