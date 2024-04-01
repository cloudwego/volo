use std::{cell::RefCell, error::Error, sync::Arc, time::Duration};

use faststr::FastStr;
use http::{
    header::{self, HeaderMap, HeaderName, HeaderValue},
    Method, Uri,
};
use metainfo::{MetaInfo, METAINFO};
use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
};
use paste::paste;
use volo::{
    client::MkClient,
    context::Context,
    net::{
        dial::{DefaultMakeTransport, MakeTransport},
        Address,
    },
};

use self::{
    meta::MetaService,
    request_builder::RequestBuilder,
    transport::{ClientConfig, ClientTransport},
    utils::IntoUri,
};
use crate::{
    context::{
        client::{CalleeName, CallerName, Config},
        ClientContext,
    },
    error::client::{builder_error, ClientError},
    request::ClientRequest,
    response::ClientResponse,
};

mod meta;
mod request_builder;
mod transport;
pub mod utils;

const PKG_NAME_WITH_VER: &str = concat!(env!("CARGO_PKG_NAME"), '/', env!("CARGO_PKG_VERSION"));

pub type ClientMetaService = MetaService<ClientTransport>;

pub struct ClientBuilder<L, MkC> {
    config: Config,
    http_config: ClientConfig,
    transport_config: volo::net::dial::Config,
    callee_name: FastStr,
    caller_name: FastStr,
    headers: HeaderMap,
    layer: L,
    mk_client: MkC,
    #[cfg(feature = "__tls")]
    tls_config: Option<volo::net::tls::TlsConnector>,
}

impl ClientBuilder<Identity, DefaultMkClient> {
    /// Create a new client builder.
    pub fn new() -> Self {
        Self {
            config: Default::default(),
            http_config: Default::default(),
            transport_config: Default::default(),
            callee_name: FastStr::empty(),
            caller_name: FastStr::empty(),
            headers: Default::default(),
            layer: Identity::new(),
            mk_client: DefaultMkClient,
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }
}

impl Default for ClientBuilder<Identity, DefaultMkClient> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L, MkC> ClientBuilder<L, MkC> {
    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn client_maker<MkC2>(self, new_mk_client: MkC2) -> ClientBuilder<L, MkC2> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            transport_config: self.transport_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            layer: self.layer,
            mk_client: new_mk_client,
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }

    /// Adds a new inner layer to the server.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer(baz)`, we will get: foo -> bar -> baz.
    pub fn layer<Inner>(self, layer: Inner) -> ClientBuilder<Stack<Inner, L>, MkC> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            transport_config: self.transport_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            layer: Stack::new(layer, self.layer),
            mk_client: self.mk_client,
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }

    /// Adds a new front layer to the server.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_front(baz)`, we will get: baz -> foo -> bar.
    pub fn layer_front<Front>(self, layer: Front) -> ClientBuilder<Stack<L, Front>, MkC> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            transport_config: self.transport_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            layer: Stack::new(self.layer, layer),
            mk_client: self.mk_client,
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }

    /// Sets the target server's name.
    pub fn callee_name<S>(&mut self, callee: S) -> &mut Self
    where
        S: Into<FastStr>,
    {
        self.callee_name = callee.into();
        self
    }

    /// Sets the client's name sent to the server.
    pub fn caller_name<S>(&mut self, caller: S) -> &mut Self
    where
        S: Into<FastStr>,
    {
        self.caller_name = caller.into();
        self
    }

    /// Insert a header to the request.
    pub fn header<K, V>(&mut self, key: K, value: V) -> Result<&mut Self, ClientError>
    where
        K: TryInto<HeaderName>,
        K::Error: Error + Send + Sync + 'static,
        V: TryInto<HeaderValue>,
        V::Error: Error + Send + Sync + 'static,
    {
        self.headers.insert(
            key.try_into().map_err(builder_error)?,
            value.try_into().map_err(builder_error)?,
        );
        Ok(self)
    }

    /// Set tls config for the client.
    #[cfg(feature = "__tls")]
    pub fn set_tls_config<T>(&mut self, tls_config: T) -> &mut Self
    where
        T: Into<volo::net::tls::TlsConnector>,
    {
        self.tls_config = Some(Into::into(tls_config));
        self
    }

    /// Get a reference to the default headers of the client.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Get a mutable reference to the default headers of the client.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// Get a reference to the HTTP configuration of the client.
    pub fn http_config(&self) -> &ClientConfig {
        &self.http_config
    }

    /// Get a mutable reference to the HTTP configuration of the client.
    pub fn http_config_mut(&mut self) -> &mut ClientConfig {
        &mut self.http_config
    }

    /// Get a reference to the transport configuration of the client.
    pub fn transport_config(&self) -> &volo::net::dial::Config {
        &self.transport_config
    }

    /// Get a mutable reference to the transport configuration of the client.
    pub fn transport_config_mut(&mut self) -> &mut volo::net::dial::Config {
        &mut self.transport_config
    }

    /// Set mode for setting `Host` in request headers, and server name when using TLS.
    ///
    /// Default is callee name.
    pub fn set_callee_name_mode(&mut self, mode: CalleeName) -> &mut Self {
        self.config.callee_name = mode;
        self
    }

    /// Set mode for setting `User-Agent` in request headers.
    ///
    /// Default is the current crate name and version.
    pub fn set_caller_name_mode(&mut self, mode: CallerName) -> &mut Self {
        self.config.caller_name = mode;
        self
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn stat_enable(&mut self, enable: bool) -> &mut Self {
        self.config.stat_enable = enable;
        self
    }

    /// Return `Err` rather than full `ClientResponse` when the response status code is 4xx or 5xx.
    ///
    /// Default is false.
    pub fn fail_on_error_status(&mut self, fail_on_error_status: bool) -> &mut Self {
        self.config.fail_on_error_status = fail_on_error_status;
        self
    }

    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Default is false.
    pub fn set_title_case_headers(&mut self, title_case_headers: bool) -> &mut Self {
        self.http_config.title_case_headers = title_case_headers;
        self
    }

    /// Set whether to support preserving original header cases.
    ///
    /// Currently, this will record the original cases received, and store them
    /// in a private extension on the `Response`. It will also look for and use
    /// such an extension in any provided `Request`.
    ///
    /// Since the relevant extension is still private, there is no way to
    /// interact with the original cases. The only effect this can have now is
    /// to forward the cases in a proxy-like fashion.
    ///
    /// Default is false.
    pub fn set_preserve_header_case(&mut self, preserve_header_case: bool) -> &mut Self {
        self.http_config.preserve_header_case = preserve_header_case;
        self
    }

    /// Set the maximum number of headers.
    ///
    /// When a response is received, the parser will reserve a buffer to store headers for optimal
    /// performance.
    ///
    /// If client receives more headers than the buffer size, the error "message header too large"
    /// is returned.
    ///
    /// Note that headers is allocated on the stack by default, which has higher performance. After
    /// setting this value, headers will be allocated in heap memory, that is, heap memory
    /// allocation will occur for each response, and there will be a performance drop of about 5%.
    ///
    /// Default is 100.
    pub fn set_max_headers(&mut self, max_headers: usize) -> &mut Self {
        self.http_config.max_headers = Some(max_headers);
        self
    }

    /// Set the maximum idle time for a connection.
    pub fn set_connect_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.transport_config.connect_timeout = Some(timeout);
        self
    }

    /// Set the maximum idle time for reading data from the connection.
    pub fn set_read_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.transport_config.read_timeout = Some(timeout);
        self
    }

    /// Set the maximum idle time for writing data to the connection.
    pub fn set_write_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.transport_config.write_timeout = Some(timeout);
        self
    }

    /// Build the HTTP client.
    pub fn build(mut self) -> MkC::Target
    where
        L: Layer<MetaService<ClientTransport>>,
        L::Service: Send + Sync + 'static,
        MkC: MkClient<Client<L::Service>>,
    {
        let mut default_mk_conn = DefaultMakeTransport::new();
        default_mk_conn.set_connect_timeout(self.transport_config.connect_timeout);
        default_mk_conn.set_read_timeout(self.transport_config.read_timeout);
        default_mk_conn.set_write_timeout(self.transport_config.write_timeout);

        let transport = ClientTransport::new(
            self.http_config,
            default_mk_conn,
            #[cfg(feature = "__tls")]
            self.tls_config.unwrap_or_default(),
        );
        let service = self.layer.layer(MetaService::new(transport));

        let caller_name = match &self.config.caller_name {
            CallerName::PkgNameWithVersion => FastStr::from_static_str(PKG_NAME_WITH_VER),
            CallerName::OriginalCallerName => self.caller_name.clone(),
            CallerName::CallerNameWithVersion if !self.caller_name.is_empty() => {
                FastStr::from_string(format!(
                    "{}/{}",
                    self.caller_name,
                    env!("CARGO_PKG_VERSION")
                ))
            }
            CallerName::Specified(val) => val.to_owned(),
            _ => FastStr::empty(),
        };

        if !caller_name.is_empty() && self.headers.get(header::USER_AGENT).is_none() {
            self.headers.insert(
                header::USER_AGENT,
                HeaderValue::from_str(caller_name.as_str()).expect("Invalid caller name"),
            );
        }

        let client_inner = ClientInner {
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            config: self.config,
        };
        let client = Client {
            transport: service,
            inner: Arc::new(client_inner),
        };
        self.mk_client.mk_client(client)
    }
}

struct ClientInner {
    callee_name: FastStr,
    caller_name: FastStr,
    headers: HeaderMap,
    config: Config,
}

#[derive(Clone)]
pub struct Client<S> {
    transport: S,
    inner: Arc<ClientInner>,
}

macro_rules! method_requests {
    ($method:ident) => {
        paste! {
            pub fn [<$method:lower>]<U>(&self, uri: U) -> Result<RequestBuilder<S>, ClientError>
            where
                U: IntoUri,
            {
                self.request(Method::[<$method:upper>], uri)
            }
        }
    };
}

impl Client<()> {
    /// Create a new client builder.
    pub fn builder() -> ClientBuilder<Identity, DefaultMkClient> {
        ClientBuilder::new()
    }
}

impl<S> Client<S> {
    /// Create a builder for building a request.
    pub fn request_builder(&self) -> RequestBuilder<S> {
        RequestBuilder::new(self)
    }

    /// Create a builder for building a request with the specified method and URI.
    pub fn request<U>(&self, method: Method, uri: U) -> Result<RequestBuilder<S>, ClientError>
    where
        U: IntoUri,
    {
        RequestBuilder::new_with_method_and_uri(self, method, uri.into_uri()?)
    }

    method_requests!(options);
    method_requests!(get);
    method_requests!(post);
    method_requests!(put);
    method_requests!(delete);
    method_requests!(head);
    method_requests!(trace);
    method_requests!(connect);
    method_requests!(patch);
}

impl<S> Client<S> {
    /// Send a request to the target address.
    ///
    /// This is a low-level method and you should build the `uri` and `request`, and get the
    /// address by yourself.
    ///
    /// For simple usage, you can use the `get`, `post` and other methods directly.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use std::net::SocketAddr;
    ///
    /// use http::{Method, Uri};
    /// use volo::net::Address;
    /// use volo_http::{body::Body, client::Client, request::ClientRequest};
    ///
    /// let client = Client::builder().build();
    /// let addr: SocketAddr = "[::]:8080".parse().unwrap();
    /// let addr = Address::from(addr);
    /// let resp = client
    ///     .send_request(
    ///         Uri::from_static("http://localhost:8080/"),
    ///         addr,
    ///         ClientRequest::builder()
    ///             .method(Method::GET)
    ///             .uri("/")
    ///             .body(Body::empty())
    ///             .expect("build request failed"),
    ///     )
    ///     .await
    ///     .expect("request failed")
    ///     .into_string()
    ///     .await
    ///     .expect("response failed to convert to string");
    /// println!("{resp:?}");
    /// ```
    pub async fn send_request<B>(
        &self,
        uri: Uri,
        target: Address,
        mut request: ClientRequest<B>,
    ) -> Result<S::Response, S::Error>
    where
        S: Service<ClientContext, ClientRequest<B>, Response = ClientResponse, Error = ClientError>
            + Send
            + Sync
            + 'static,
        B: Send + 'static,
    {
        let caller_name = self.inner.caller_name.clone();
        let callee_name = match self.inner.config.callee_name {
            CalleeName::TargetName => match uri.host() {
                // IPv6 address in URI has square brackets, but we does not need it as a
                // "host name".
                Some(host) => FastStr::from(
                    host.trim_start_matches('[')
                        .trim_end_matches(']')
                        .to_owned(),
                ),
                None => match &target {
                    Address::Ip(addr) => FastStr::from(addr.ip().to_string()),
                    #[cfg(target_family = "unix")]
                    Address::Unix(_) => FastStr::empty(),
                },
            },
            CalleeName::OriginalCalleeName => self.inner.callee_name.clone(),
            CalleeName::None => FastStr::empty(),
        };
        tracing::trace!(
            "create a request with caller_name: {caller_name}, callee_name: {callee_name}"
        );

        if request.headers().get(header::HOST).is_none() && uri.host().is_some() {
            let mut host = uri.host().unwrap().to_string();
            if let Some(port) = uri.port() {
                host.push(':');
                host.push_str(port.as_str());
            }
            if let Ok(value) = HeaderValue::from_str(&host) {
                request.headers_mut().insert(header::HOST, value);
            } else {
                tracing::info!(
                    "failed to insert `Host` to headers, `{host}` is not a valid header value"
                );
            }
        }

        let mut cx = ClientContext::new(
            target,
            #[cfg(feature = "__tls")]
            uri.scheme()
                .is_some_and(|scheme| scheme == &http::uri::Scheme::HTTPS),
        );
        cx.rpc_info_mut().caller_mut().set_service_name(caller_name);
        cx.rpc_info_mut().callee_mut().set_service_name(callee_name);
        cx.rpc_info_mut().set_config(self.inner.config.clone());

        self.call(&mut cx, request).await
    }
}

impl<S, B> Service<ClientContext, ClientRequest<B>> for Client<S>
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
        req.headers_mut().extend(self.inner.headers.clone());

        let has_metainfo = METAINFO.try_with(|_| {}).is_ok();
        let mk_call = self.transport.call(cx, req);

        if has_metainfo {
            mk_call.await
        } else {
            METAINFO
                .scope(RefCell::new(MetaInfo::default()), mk_call)
                .await
        }
    }
}

pub struct DefaultMkClient;

impl<S> MkClient<Client<S>> for DefaultMkClient {
    type Target = Client<S>;

    fn mk_client(&self, service: Client<S>) -> Self::Target {
        service
    }
}

/// Send a GET request to the specified URI.
pub async fn get<U>(uri: U) -> Result<ClientResponse, ClientError>
where
    U: IntoUri,
{
    ClientBuilder::new().build().get(uri)?.send().await
}
