use std::{cell::RefCell, error::Error, sync::Arc, time::Duration};

use faststr::FastStr;
use http::{
    header::{self, HeaderMap, HeaderName, HeaderValue},
    uri::{Scheme, Uri},
    Method,
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
    loadbalance::MkLbLayer,
    net::{
        dial::{DefaultMakeTransport, MakeTransport},
        Address,
    },
};

use self::{
    loadbalance::{DefaultLB, LbConfig},
    meta::MetaService,
    request_builder::RequestBuilder,
    transport::{ClientConfig, ClientTransport},
    utils::{Target, TargetBuilder},
};
use crate::{
    context::{
        client::{CalleeName, CallerName, Config},
        ClientContext,
    },
    error::{
        client::{builder_error, ClientError},
        BoxError,
    },
    request::ClientRequest,
    response::ClientResponse,
};

pub mod loadbalance;
mod meta;
mod request_builder;
mod transport;
pub mod utils;

const PKG_NAME_WITH_VER: &str = concat!(env!("CARGO_PKG_NAME"), '/', env!("CARGO_PKG_VERSION"));

pub type ClientMetaService = MetaService<ClientTransport>;
pub type DefaultClient = Client<ClientMetaService>;

pub struct ClientBuilder<L, MkC, LB> {
    config: Config,
    http_config: ClientConfig,
    connector: DefaultMakeTransport,
    callee_name: FastStr,
    caller_name: FastStr,
    target: TargetBuilder,
    headers: HeaderMap,
    layer: L,
    mk_client: MkC,
    mk_lb: LB,
    #[cfg(feature = "__tls")]
    tls_config: Option<volo::net::tls::TlsConnector>,
}

impl ClientBuilder<Identity, DefaultMkClient, DefaultLB> {
    /// Create a new client builder.
    pub fn new() -> Self {
        Self {
            config: Default::default(),
            http_config: Default::default(),
            connector: Default::default(),
            callee_name: FastStr::empty(),
            caller_name: FastStr::empty(),
            target: Default::default(),
            headers: Default::default(),
            layer: Identity::new(),
            mk_client: DefaultMkClient,
            mk_lb: Default::default(),
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }
}

impl Default for ClientBuilder<Identity, DefaultMkClient, DefaultLB> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L, MkC, LB, DISC> ClientBuilder<L, MkC, LbConfig<LB, DISC>> {
    /// Set load balancer for the client.
    pub fn load_balance<NLB>(
        self,
        load_balance: NLB,
    ) -> ClientBuilder<L, MkC, LbConfig<NLB, DISC>> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            headers: self.headers,
            layer: self.layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.load_balance(load_balance),
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Set service discover for the client.
    pub fn discover<NDISC>(self, discover: NDISC) -> ClientBuilder<L, MkC, LbConfig<LB, NDISC>> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            headers: self.headers,
            layer: self.layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.discover(discover),
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }
}

impl<L, MkC, LB> ClientBuilder<L, MkC, LB> {
    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn client_maker<MkC2>(self, new_mk_client: MkC2) -> ClientBuilder<L, MkC2, LB> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            headers: self.headers,
            layer: self.layer,
            mk_client: new_mk_client,
            mk_lb: self.mk_lb,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Add a new inner layer to the client.
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
    pub fn layer<Inner>(self, layer: Inner) -> ClientBuilder<Stack<Inner, L>, MkC, LB> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            headers: self.headers,
            layer: Stack::new(layer, self.layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Add a new front layer to the client.
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
    pub fn layer_front<Front>(self, layer: Front) -> ClientBuilder<Stack<L, Front>, MkC, LB> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            headers: self.headers,
            layer: Stack::new(self.layer, layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    pub fn mk_load_balance<NLB>(self, mk_load_balance: NLB) -> ClientBuilder<L, MkC, NLB> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            headers: self.headers,
            layer: self.layer,
            mk_client: self.mk_client,
            mk_lb: mk_load_balance,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Set the target server's name.
    pub fn callee_name<S>(&mut self, callee: S) -> &mut Self
    where
        S: Into<FastStr>,
    {
        self.callee_name = callee.into();
        self
    }

    /// Set the client's name sent to the server.
    pub fn caller_name<S>(&mut self, caller: S) -> &mut Self
    where
        S: Into<FastStr>,
    {
        self.caller_name = caller.into();
        self
    }

    /// Set the target address of the client.
    ///
    /// If there is no target specified when building a request, client will use this address.
    pub fn address<A>(&mut self, address: A, #[cfg(feature = "__tls")] use_tls: bool) -> &mut Self
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

    /// Set the target host of the client.
    ///
    /// If there is no target specified when building a request, client will use this address.
    ///
    /// If tls is enabled, the scheme will be set to `https`, otherwise it will be set to `http`.
    ///
    /// To specify scheme or port, use `scheme_host_and_port` instead.
    pub fn host<H>(&mut self, host: H) -> &mut Self
    where
        H: Into<FastStr>,
    {
        self.target = TargetBuilder::Host {
            scheme: None,
            host: host.into(),
            port: None,
        };
        self
    }

    /// Set the target scheme, host and port of the client.
    ///
    /// If there is no target specified when building a request, client will use this address.
    pub fn scheme_host_and_port<H>(
        &mut self,
        scheme: Option<Scheme>,
        host: H,
        port: Option<u16>,
    ) -> &mut Self
    where
        H: Into<FastStr>,
    {
        self.target = TargetBuilder::Host {
            scheme,
            host: host.into(),
            port,
        };
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

    /// Disable TLS for the client.
    ///
    /// Default is false, when TLS related feature is enabled, TLS is enabled by default.
    #[cfg(feature = "__tls")]
    pub fn disable_tls(&mut self, disable: bool) -> &mut Self {
        self.config.disable_tls = disable;
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
        self.connector.set_connect_timeout(Some(timeout));
        self
    }

    /// Set the maximum idle time for reading data from the connection.
    pub fn set_read_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.connector.set_read_timeout(Some(timeout));
        self
    }

    /// Set the maximum idle time for writing data to the connection.
    pub fn set_write_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.connector.set_write_timeout(Some(timeout));
        self
    }

    /// Build the HTTP client.
    pub fn build(mut self) -> MkC::Target
    where
        L: Layer<MetaService<ClientTransport>>,
        L::Service: Send + Sync + 'static,
        LB: MkLbLayer,
        LB::Layer: Layer<L::Service>,
        <LB::Layer as Layer<L::Service>>::Service: Send + Sync,
        MkC: MkClient<Client<<LB::Layer as Layer<L::Service>>::Service>>,
    {
        let service = MetaService::new(ClientTransport::new(
            self.http_config,
            self.connector,
            #[cfg(feature = "__tls")]
            self.tls_config.unwrap_or_default(),
        ));
        let service = self.mk_lb.make().layer(self.layer.layer(service));

        let caller_name = match &self.config.caller_name {
            CallerName::PkgNameWithVersion => FastStr::from_static_str(PKG_NAME_WITH_VER),
            CallerName::OriginalCallerName => self.caller_name,
            CallerName::CallerNameWithVersion if !self.caller_name.is_empty() => {
                FastStr::from_string(format!(
                    "{}/{}",
                    self.caller_name,
                    env!("CARGO_PKG_VERSION")
                ))
            }
            CallerName::Specified(val) => val.clone(),
            _ => FastStr::empty(),
        };
        if !caller_name.is_empty() && self.headers.get(header::USER_AGENT).is_none() {
            self.headers.insert(
                header::USER_AGENT,
                HeaderValue::from_str(caller_name.as_str()).expect("Invalid caller name"),
            );
        }

        #[cfg(feature = "__tls")]
        let default_target_is_tls = self.target.is_tls();
        let default_target_callee_name = self
            .target
            .gen_callee_name(&self.config.callee_name, &self.callee_name);
        let default_target = if self.target.is_none() {
            None
        } else {
            Some(Target {
                addr: self
                    .target
                    .resolve_sync()
                    .expect("failed to resolve default target of client"),
                #[cfg(feature = "__tls")]
                use_tls: default_target_is_tls,
                callee_name: default_target_callee_name,
            })
        };

        let client_inner = ClientInner {
            callee_name: self.callee_name,
            caller_name,
            default_target,
            headers: self.headers,
            config: self.config,
        };
        let client = Client {
            service,
            inner: Arc::new(client_inner),
        };
        self.mk_client.mk_client(client)
    }
}

pub(super) struct ClientInner {
    callee_name: FastStr,
    caller_name: FastStr,
    default_target: Option<Target>,
    headers: HeaderMap,
    config: Config,
}

#[derive(Clone)]
pub struct Client<S> {
    service: S,
    inner: Arc<ClientInner>,
}

macro_rules! method_requests {
    ($method:ident) => {
        paste! {
            pub fn [<$method:lower>]<U>(&self, uri: U) -> Result<RequestBuilder<S>, ClientError>
            where
                U: TryInto<Uri>,
                U::Error: Into<BoxError>,
            {
                self.request(Method::[<$method:upper>], uri)
            }
        }
    };
}

impl Client<()> {
    /// Create a new client builder.
    pub fn builder() -> ClientBuilder<Identity, DefaultMkClient, DefaultLB> {
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
        U: TryInto<Uri>,
        U::Error: Into<BoxError>,
    {
        RequestBuilder::new_with_method_and_uri(
            self,
            method,
            uri.try_into().map_err(builder_error)?,
        )
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

    /// Get the default target address of the client.
    pub fn default_target(&self) -> Option<&Target> {
        self.inner.default_target.as_ref()
    }

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
    /// use volo_http::client::utils::TargetBuilder;
    ///
    /// let client = Client::builder().build();
    /// let addr: SocketAddr = "[::]:8080".parse().unwrap();
    /// let addr = Address::from(addr);
    /// let resp = client
    ///     .send_request(
    ///         TargetBuilder::Address { addr },
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
        target: TargetBuilder,
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
        let target = target
            .into_target(&self.inner)
            .await?
            .or_else(|| self.inner.default_target.clone());

        let (callee_name, mut cx) = match target {
            Some(target) => {
                let mut cx = ClientContext::new(
                    #[cfg(feature = "__tls")]
                    target.use_tls,
                );
                cx.rpc_info_mut().callee_mut().set_address(target.addr);
                (target.callee_name, cx)
            }
            None => (
                self.inner.callee_name.clone(),
                ClientContext::new(
                    #[cfg(feature = "__tls")]
                    !self.inner.config.disable_tls,
                ),
            ),
        };

        tracing::trace!(
            "create a request with caller_name: {caller_name}, callee_name: {callee_name}"
        );

        cx.rpc_info_mut().caller_mut().set_service_name(caller_name);
        cx.rpc_info_mut()
            .callee_mut()
            .set_service_name(callee_name.clone());
        cx.rpc_info_mut().set_config(self.inner.config.clone());

        if let Ok(host) = HeaderValue::from_maybe_shared(callee_name) {
            request.headers_mut().insert(header::HOST, host);
        }

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
        let stat_enabled = cx.stat_enabled();

        let mk_call = async {
            if stat_enabled {
                cx.common_stats.record_process_start_at();
            }

            let res = self.service.call(cx, req).await;

            if stat_enabled {
                cx.common_stats.record_process_end_at();
            }
            res
        };

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

/// Create a GET request to the specified URI.
pub async fn get<U>(uri: U) -> Result<ClientResponse, ClientError>
where
    U: TryInto<Uri>,
    U::Error: Into<BoxError>,
{
    ClientBuilder::new().build().get(uri)?.send().await
}
