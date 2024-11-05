//! Request builder for building a request and sending to server
//!
//! See [`RequestBuilder`] for more details.

use std::error::Error;

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
pub struct RequestBuilder<S, B = Body> {
    client: Client<S>,
    target: Target,
    call_opt: Option<CallOpt>,
    request: Result<ClientRequest<B>>,
}

impl<S> RequestBuilder<S, Body> {
    pub(crate) fn new(client: Client<S>) -> Self {
        Self {
            client,
            target: Default::default(),
            call_opt: Default::default(),
            request: Ok(ClientRequest::default()),
        }
    }

    /// Set the request body.
    pub fn data<D>(mut self, data: D) -> Self
    where
        D: TryInto<Body>,
        D::Error: Error + Send + Sync + 'static,
    {
        if self.request.is_err() {
            return self;
        }
        let Ok(req) = self.request else {
            unreachable!();
        };

        let body = match data.try_into() {
            Ok(body) => body,
            Err(err) => {
                self.request = Err(builder_error(err));
                return self;
            }
        };

        let (parts, _) = req.into_parts();
        self.request = Ok(Request::from_parts(parts, body));

        self
    }

    /// Set the request body as json from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "json")]
    pub fn json<T>(mut self, json: &T) -> Self
    where
        T: serde::Serialize,
    {
        if self.request.is_err() {
            return self;
        }
        let Ok(req) = self.request else {
            unreachable!();
        };

        let json = match crate::utils::json::serialize(json) {
            Ok(json) => json,
            Err(err) => {
                self.request = Err(builder_error(err));
                return self;
            }
        };

        let (mut parts, _) = req.into_parts();
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            crate::utils::consts::APPLICATION_JSON,
        );
        self.request = Ok(Request::from_parts(parts, Body::from(json)));

        self
    }

    /// Set the request body as form from object with [`Serialize`](serde::Serialize).
    #[cfg(feature = "form")]
    pub fn form<T>(mut self, form: &T) -> Self
    where
        T: serde::Serialize,
    {
        if self.request.is_err() {
            return self;
        }
        let Ok(req) = self.request else {
            unreachable!();
        };

        let form = match serde_urlencoded::to_string(form) {
            Ok(form) => form,
            Err(err) => {
                self.request = Err(builder_error(err));
                return self;
            }
        };

        let (mut parts, _) = req.into_parts();
        parts.headers.insert(
            http::header::CONTENT_TYPE,
            crate::utils::consts::APPLICATION_WWW_FORM_URLENCODED,
        );
        self.request = Ok(Request::from_parts(parts, Body::from(form)));

        self
    }
}

impl<S, B> RequestBuilder<S, B> {
    /// Set method for the request.
    pub fn method(mut self, method: Method) -> Self {
        if let Ok(req) = self.request.as_mut() {
            *req.method_mut() = method;
        }
        self
    }

    /// Get a reference to method in the request.
    pub fn method_ref(&self) -> Option<&Method> {
        self.request.as_ref().ok().map(Request::method)
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
        if self.request.is_err() {
            return self;
        }
        let uri = match uri.try_into() {
            Ok(uri) => uri,
            Err(err) => {
                self.request = Err(builder_error(err));
                return self;
            }
        };
        if let Some(target) = Target::from_uri(&uri) {
            match target {
                Ok(target) => self.target = target,
                Err(err) => {
                    self.request = Err(err);
                    return self;
                }
            }
        }
        let rela_uri = uri
            .path_and_query()
            .map(PathAndQuery::to_owned)
            .unwrap_or_else(|| PathAndQuery::from_static("/"))
            .into();
        let Ok(req) = self.request.as_mut() else {
            unreachable!();
        };
        *req.uri_mut() = rela_uri;

        self
    }

    /// Set full uri for building request.
    ///
    /// In this function, scheme and host will be resolved as the target address, and the full uri
    /// will be set as the request uri.
    ///
    /// This function is only used for using http(s) proxy.
    pub fn full_uri<U>(mut self, uri: U) -> Self
    where
        U: TryInto<Uri>,
        U::Error: Into<BoxError>,
    {
        if self.request.is_err() {
            return self;
        }
        let uri = match uri.try_into() {
            Ok(uri) => uri,
            Err(err) => {
                self.request = Err(builder_error(err));
                return self;
            }
        };
        if let Some(target) = Target::from_uri(&uri) {
            match target {
                Ok(target) => self.target = target,
                Err(err) => {
                    self.request = Err(err);
                    return self;
                }
            }
        }
        let Ok(req) = self.request.as_mut() else {
            unreachable!();
        };
        *req.uri_mut() = uri;

        self
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
    pub fn set_query<T>(mut self, query: &T) -> Self
    where
        T: serde::Serialize,
    {
        if self.request.is_err() {
            return self;
        }
        let query_str = match serde_urlencoded::to_string(query) {
            Ok(query) => query,
            Err(err) => {
                self.request = Err(builder_error(err));
                return self;
            }
        };
        let Ok(req) = self.request.as_mut() else {
            unreachable!();
        };

        // We should keep path only without query
        let path_str = req.uri().path();
        let mut path = String::with_capacity(path_str.len() + 1 + query_str.len());
        path.push_str(path_str);
        path.push('?');
        path.push_str(&query_str);
        let Ok(uri) = Uri::from_maybe_shared(path) else {
            // path part is from a valid uri, and the result of urlencoded must be valid.
            unreachable!();
        };

        *req.uri_mut() = uri;

        self
    }

    /// Get a reference to uri in the request.
    pub fn uri_ref(&self) -> Option<&Uri> {
        self.request.as_ref().ok().map(Request::uri)
    }

    /// Set version of the HTTP request.
    pub fn version(mut self, version: Version) -> Self {
        if let Ok(req) = self.request.as_mut() {
            *req.version_mut() = version;
        }
        self
    }

    /// Get a reference to version in the request.
    pub fn version_ref(&self) -> Option<Version> {
        self.request.as_ref().ok().map(Request::version)
    }

    /// Insert a header into the request header map.
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        K: TryInto<HeaderName>,
        K::Error: Into<http::Error>,
        V: TryInto<HeaderValue>,
        V::Error: Into<http::Error>,
    {
        if self.request.is_err() {
            return self;
        }

        let key = match key.try_into() {
            Ok(key) => key,
            Err(err) => {
                self.request = Err(builder_error(err.into()));
                return self;
            }
        };
        let value = match value.try_into() {
            Ok(value) => value,
            Err(err) => {
                self.request = Err(builder_error(err.into()));
                return self;
            }
        };

        let Ok(req) = self.request.as_mut() else {
            unreachable!();
        };

        req.headers_mut().insert(key, value);

        self
    }

    /// Get a reference to headers in the request.
    pub fn headers(&self) -> Option<&HeaderMap> {
        self.request.as_ref().ok().map(Request::headers)
    }

    /// Get a mutable reference to headers in the request.
    pub fn headers_mut(&mut self) -> Option<&mut HeaderMap> {
        self.request.as_mut().ok().map(Request::headers_mut)
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
    pub fn body<B2>(self, body: B2) -> RequestBuilder<S, B2> {
        let request = match self.request {
            Ok(req) => {
                let (parts, _) = req.into_parts();
                Ok(Request::from_parts(parts, body))
            }
            Err(err) => Err(err),
        };

        RequestBuilder {
            client: self.client,
            target: self.target,
            call_opt: self.call_opt,
            request,
        }
    }

    /// Get a reference to body in the request.
    pub fn body_ref(&self) -> Option<&B> {
        self.request.as_ref().ok().map(Request::body)
    }
}

impl<S, B> RequestBuilder<S, B>
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
            .send_request(self.target, self.call_opt, self.request?)
            .await
    }
}

// The `httpbin.org` always responses a json data.
#[cfg(feature = "json")]
#[cfg(test)]
mod request_tests {
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
        #[serde(default)]
        form: HashMap<String, String>,
        #[serde(default)]
        json: Option<HashMap<String, String>>,
    }

    fn test_data() -> HashMap<String, String> {
        HashMap::from([
            ("key1".to_string(), "val1".to_string()),
            ("key2".to_string(), "val2".to_string()),
        ])
    }

    #[cfg(feature = "query")]
    #[tokio::test]
    async fn set_query() {
        let data = test_data();

        let client = Client::builder().build();
        let resp = client
            .get("http://httpbin.org/get")
            .set_query(&data)
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert_eq!(resp.args, data);
    }

    #[cfg(feature = "form")]
    #[tokio::test]
    async fn set_form() {
        let data = test_data();

        let client = Client::builder().build();
        let resp = client
            .post("http://httpbin.org/post")
            .form(&data)
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert_eq!(resp.form, data);
    }

    #[tokio::test]
    async fn set_json() {
        let data = test_data();

        let client = Client::builder().build();
        let resp = client
            .post("http://httpbin.org/post")
            .json(&data)
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert_eq!(resp.json, Some(data));
    }
}
