//! Client implementation
//!
//! See [`Client`] for more details.

use std::{cell::RefCell, error::Error, sync::Arc, time::Duration};

use faststr::FastStr;
use http::{
    header::{self, HeaderMap, HeaderName, HeaderValue},
    uri::Uri,
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

#[cfg(feature = "__tls")]
#[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
pub use self::transport::TlsTransport;
use self::{
    callopt::CallOpt,
    dns::parse_target,
    loadbalance::{DefaultLB, DefaultLBService, LbConfig},
    meta::MetaService,
    target::TargetParser,
    transport::{ClientConfig, ClientTransport, ClientTransportConfig},
};
use crate::{
    context::{client::Config, ClientContext},
    error::{
        client::{builder_error, no_address, ClientError, Result},
        BoxError,
    },
    request::ClientRequest,
    response::ClientResponse,
};

pub mod callopt;
#[cfg(feature = "cookie")]
pub mod cookie;
pub mod dns;
pub mod loadbalance;
mod meta;
mod request_builder;
pub mod target;
#[cfg(test)]
pub mod test_helpers;
mod transport;

pub use self::{request_builder::RequestBuilder, target::Target};

#[doc(hidden)]
pub mod prelude {
    pub use super::{Client, ClientBuilder};
}

const PKG_NAME_WITH_VER: &str = concat!(env!("CARGO_PKG_NAME"), '/', env!("CARGO_PKG_VERSION"));

/// Default inner service of [`Client`]
pub type ClientMetaService = MetaService<ClientTransport>;
/// Default [`Client`] without any extra [`Layer`]s
pub type DefaultClient<IL = Identity, OL = Identity> =
Client<<OL as Layer<DefaultLBService<<IL as Layer<ClientMetaService>>::Service>>>::Service>;

/// A builder for configuring an HTTP [`Client`].
pub struct ClientBuilder<IL, OL, C, LB> {
    http_config: ClientConfig,
    builder_config: BuilderConfig,
    connector: DefaultMakeTransport,
    callee_name: FastStr,
    caller_name: FastStr,
    target: Target,
    call_opt: Option<CallOpt>,
    target_parser: TargetParser,
    headers: HeaderMap,
    inner_layer: IL,
    outer_layer: OL,
    mk_client: C,
    mk_lb: LB,
    #[cfg(feature = "__tls")]
    tls_config: Option<volo::net::tls::TlsConnector>,
}

/// Configuration for [`ClientBuilder`]
///
/// This is unstable now and may be changed in the future.
#[doc(hidden)]
pub struct BuilderConfig {
    pub timeout: Option<Duration>,
    pub stat_enable: bool,
    pub fail_on_error_status: bool,
    #[cfg(feature = "__tls")]
    pub disable_tls: bool,
}

impl Default for BuilderConfig {
    fn default() -> Self {
        Self {
            timeout: None,
            stat_enable: true,
            fail_on_error_status: false,
            #[cfg(feature = "__tls")]
            disable_tls: false,
        }
    }
}

impl ClientBuilder<Identity, Identity, DefaultMkClient, DefaultLB> {
    /// Create a new client builder.
    pub fn new() -> Self {
        Self {
            http_config: Default::default(),
            builder_config: Default::default(),
            connector: Default::default(),
            callee_name: FastStr::empty(),
            caller_name: FastStr::empty(),
            target: Default::default(),
            call_opt: Default::default(),
            target_parser: parse_target,
            headers: Default::default(),
            inner_layer: Identity::new(),
            outer_layer: Identity::new(),
            mk_client: DefaultMkClient,
            mk_lb: Default::default(),
            #[cfg(feature = "__tls")]
            tls_config: None,
        }
    }
}

impl Default for ClientBuilder<Identity, Identity, DefaultMkClient, DefaultLB> {
    fn default() -> Self {
        Self::new()
    }
}

impl<IL, OL, C, LB, DISC> ClientBuilder<IL, OL, C, LbConfig<LB, DISC>> {
    /// Set load balancer for the client.
    pub fn load_balance<NLB>(
        self,
        load_balance: NLB,
    ) -> ClientBuilder<IL, OL, C, LbConfig<NLB, DISC>> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.load_balance(load_balance),
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Set service discover for the client.
    pub fn discover<NDISC>(self, discover: NDISC) -> ClientBuilder<IL, OL, C, LbConfig<LB, NDISC>> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.discover(discover),
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }
}

impl<IL, OL, C, LB> ClientBuilder<IL, OL, C, LB> {
    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn client_maker<C2>(self, new_mk_client: C2) -> ClientBuilder<IL, OL, C2, LB> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
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
    /// After we call `.layer_inner(baz)`, we will get: foo -> bar -> baz.
    ///
    /// The overall order for layers is: outer -> LoadBalance -> \[inner\] -> transport.
    pub fn layer_inner<Inner>(self, layer: Inner) -> ClientBuilder<Stack<Inner, IL>, OL, C, LB> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: Stack::new(layer, self.inner_layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
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
    /// After we call `.layer_inner_front(baz)`, we will get: baz -> foo -> bar.
    ///
    /// The overall order for layers is: outer -> LoadBalance -> \[inner\] -> transport.
    pub fn layer_inner_front<Inner>(
        self,
        layer: Inner,
    ) -> ClientBuilder<Stack<IL, Inner>, OL, C, LB> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: Stack::new(self.inner_layer, layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Add a new outer layer to the client.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_outer(baz)`, we will get: foo -> bar -> baz.
    ///
    /// The overall order for layers is: \[outer\] -> Timeout -> LoadBalance -> inner -> transport.
    pub fn layer_outer<Outer>(self, layer: Outer) -> ClientBuilder<IL, Stack<Outer, OL>, C, LB> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(layer, self.outer_layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Add a new outer layer to the client.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_outer_front(baz)`, we will get: baz -> foo -> bar.
    ///
    /// The overall order for layers is: \[outer\] -> LoadBalance -> inner -> transport.
    pub fn layer_outer_front<Outer>(
        self,
        layer: Outer,
    ) -> ClientBuilder<IL, Stack<OL, Outer>, C, LB> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(self.outer_layer, layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Set a new load balance for the client.
    pub fn mk_load_balance<NLB>(self, mk_load_balance: NLB) -> ClientBuilder<IL, OL, C, NLB> {
        ClientBuilder {
            http_config: self.http_config,
            builder_config: self.builder_config,
            connector: self.connector,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            target: self.target,
            call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: mk_load_balance,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Set the target server's name.
    ///
    /// When sending a request, the `Host` in headers will be the host name or host address by
    /// default. If the callee name is not empty, it can override the default `Host`.
    ///
    /// Default is empty, the default `Host` will be used.
    pub fn callee_name<S>(&mut self, callee: S) -> &mut Self
    where
        S: AsRef<str>,
    {
        self.callee_name = FastStr::from_string(callee.as_ref().to_owned());
        self
    }

    /// Set the client's name sent to the server.
    ///
    /// When sending a request, the `User-Agent` in headers will be the crate name with its
    /// version. If the caller name is not empty, it can override the default `User-Agent`.
    ///
    /// Default is empty, the default `User-Agnet` will be used.
    pub fn caller_name<S>(&mut self, caller: S) -> &mut Self
    where
        S: AsRef<str>,
    {
        self.caller_name = FastStr::from_string(caller.as_ref().to_owned());
        self
    }

    /// Set the target address of the client.
    ///
    /// If there is no target specified when building a request, client will use this address.
    pub fn address<A>(&mut self, address: A) -> &mut Self
    where
        A: Into<Address>,
    {
        self.target = Target::from_address(address);
        self
    }

    /// Set the target host of the client.
    ///
    /// If there is no target specified when building a request, client will use this address.
    ///
    /// It uses http with port 80 by default.
    ///
    /// For setting scheme and port, use [`Self::with_port`] and [`Self::with_https`] after
    /// specifying host.
    pub fn host<H>(&mut self, host: H) -> &mut Self
    where
        H: AsRef<str>,
    {
        self.target = Target::from_host(host);
        self
    }

    /// Set the port of the default target.
    ///
    /// If there is no target specified, the function will do nothing.
    pub fn with_port(&mut self, port: u16) -> &mut Self {
        self.target.set_port(port);
        self
    }

    /// Set if the default target uses https for transporting.
    #[cfg(feature = "__tls")]
    pub fn with_https(&mut self, https: bool) -> &mut Self {
        self.target.set_https(https);
        self
    }

    /// Set a [`CallOpt`] to the client as default options for the default target.
    ///
    /// The [`CallOpt`] is used for service discover, default is an empty one.
    ///
    /// See [`CallOpt`] for more details.
    pub fn with_callopt(&mut self, call_opt: CallOpt) -> &mut Self {
        self.call_opt = Some(call_opt);
        self
    }

    /// Set a target parser for parsing `Target` and updating `Endpoint`.
    ///
    /// The `TargetParser` usually used for service discover, it can update `Endpoint` from
    /// `Target` and the service discover will resolve the `Endpoint` to `Address`.
    pub fn target_parser(&mut self, target_parser: TargetParser) -> &mut Self {
        self.target_parser = target_parser;
        self
    }

    /// Insert a header to the request.
    pub fn header<K, V>(&mut self, key: K, value: V) -> Result<&mut Self>
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

    /// Get a reference of [`Target`].
    pub fn target_ref(&self) -> &Target {
        &self.target
    }

    /// Get a mutable reference of [`Target`].
    pub fn target_mut(&mut self) -> &mut Target {
        &mut self.target
    }

    /// Get a reference of [`CallOpt`].
    pub fn callopt_ref(&self) -> &Option<CallOpt> {
        &self.call_opt
    }

    /// Get a mutable reference of [`CallOpt`].
    pub fn callopt_mut(&mut self) -> &mut Option<CallOpt> {
        &mut self.call_opt
    }

    /// Set tls config for the client.
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
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

    /// Get a reference to HTTP configuration of the client.
    pub fn http_config_ref(&self) -> &ClientConfig {
        &self.http_config
    }

    /// Get a mutable reference to HTTP configuration of the client.
    pub fn http_config_mut(&mut self) -> &mut ClientConfig {
        &mut self.http_config
    }

    /// Get a reference to builder configuration of the client.
    pub fn builder_config_ref(&self) -> &BuilderConfig {
        &self.builder_config
    }

    /// Get a mutable reference to builder configuration of the client.
    pub fn builder_config_mut(&mut self) -> &mut BuilderConfig {
        &mut self.builder_config
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn stat_enable(&mut self, enable: bool) -> &mut Self {
        self.builder_config.stat_enable = enable;
        self
    }

    /// Return `Err` rather than full `ClientResponse` when the response status code is 4xx or 5xx.
    ///
    /// Default is false.
    pub fn fail_on_error_status(&mut self, fail_on_error_status: bool) -> &mut Self {
        self.builder_config.fail_on_error_status = fail_on_error_status;
        self
    }

    /// Disable TLS for the client.
    ///
    /// Default is false, when TLS related feature is enabled, TLS is enabled by default.
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn disable_tls(&mut self, disable: bool) -> &mut Self {
        self.builder_config.disable_tls = disable;
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

    /// Set the maximin idle time for the request.
    ///
    /// The whole request includes connecting, writting, and reading the whole HTTP protocol
    /// headers (without reading response body).
    pub fn set_request_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.builder_config.timeout = Some(timeout);
        self
    }

    /// Build the HTTP client.
    pub fn build(mut self) -> C::Target
    where
        IL: Layer<MetaService<ClientTransport>>,
        IL::Service: Send + Sync + 'static,
        LB: MkLbLayer,
        LB::Layer: Layer<IL::Service>,
        <LB::Layer as Layer<IL::Service>>::Service: Send + Sync,
        OL: Layer<<LB::Layer as Layer<IL::Service>>::Service>,
        OL::Service: Send + Sync + 'static,
        C: MkClient<Client<OL::Service>>,
    {
        let transport_config = ClientTransportConfig {
            stat_enable: self.builder_config.stat_enable,
            #[cfg(feature = "__tls")]
            disable_tls: self.builder_config.disable_tls,
        };
        let transport = ClientTransport::new(
            self.http_config,
            transport_config,
            self.connector,
            #[cfg(feature = "__tls")]
            self.tls_config.unwrap_or_default(),
        );
        let meta_service = MetaService::new(transport);
        let service = self.outer_layer.layer(
            self.mk_lb
                .make()
                .layer(self.inner_layer.layer(meta_service)),
        );

        let caller_name = if self.caller_name.is_empty() {
            FastStr::from_static_str(PKG_NAME_WITH_VER)
        } else {
            self.caller_name
        };
        if !caller_name.is_empty() && self.headers.get(header::USER_AGENT).is_none() {
            self.headers.insert(
                header::USER_AGENT,
                HeaderValue::from_str(caller_name.as_str()).expect("Invalid caller name"),
            );
        }
        let config = Config {
            timeout: self.builder_config.timeout,
            fail_on_error_status: self.builder_config.fail_on_error_status,
        };

        let client_inner = ClientInner {
            service,
            caller_name,
            callee_name: self.callee_name,
            default_target: self.target,
            default_config: config,
            default_call_opt: self.call_opt,
            target_parser: self.target_parser,
            headers: self.headers,
        };
        let client = Client {
            inner: Arc::new(client_inner),
        };
        self.mk_client.mk_client(client)
    }
}

struct ClientInner<S> {
    service: S,
    caller_name: FastStr,
    callee_name: FastStr,
    default_target: Target,
    default_config: Config,
    default_call_opt: Option<CallOpt>,
    target_parser: TargetParser,
    headers: HeaderMap,
}

/// An Client for sending HTTP requests and handling HTTP responses.
///
/// # Examples
///
/// ```
/// use volo_http::{body::BodyConversion, client::Client};
///
/// # tokio_test::block_on(async {
/// let client = Client::builder().build();
/// let resp = client
///     .get("http://httpbin.org/get")
///     .send()
///     .await
///     .expect("failed to send request")
///     .into_string()
///     .await
///     .expect("failed to convert response to string");
/// println!("{resp:?}");
/// # })
/// ```
pub struct Client<S> {
    inner: Arc<ClientInner<S>>,
}

impl<S> Clone for Client<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

macro_rules! method_requests {
    ($method:ident) => {
        paste! {
            #[doc = concat!("Create a request with `", stringify!([<$method:upper>]) ,"` method and the given `uri`.")]
            pub fn [<$method:lower>]<U>(&self, uri: U) -> RequestBuilder<S>
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
    pub fn builder() -> ClientBuilder<Identity, Identity, DefaultMkClient, DefaultLB> {
        ClientBuilder::new()
    }
}

impl<S> Client<S> {
    /// Create a builder for building a request.
    pub fn request_builder(&self) -> RequestBuilder<S> {
        RequestBuilder::new(self.clone())
    }

    /// Create a builder for building a request with the specified method and URI.
    pub fn request<U>(&self, method: Method, uri: U) -> RequestBuilder<S>
    where
        U: TryInto<Uri>,
        U::Error: Into<BoxError>,
    {
        RequestBuilder::new(self.clone()).method(method).uri(uri)
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
    pub fn default_target(&self) -> &Target {
        &self.inner.default_target
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
    /// ```no_run
    /// use std::net::SocketAddr;
    ///
    /// use http::{Method, Uri};
    /// use volo::net::Address;
    /// use volo_http::{
    ///     body::{Body, BodyConversion},
    ///     client::{Client, Target},
    ///     request::ClientRequest,
    /// };
    ///
    /// # tokio_test::block_on(async {
    /// let client = Client::builder().build();
    /// let addr: SocketAddr = "[::]:8080".parse().unwrap();
    /// let resp = client
    ///     .send_request(
    ///         Target::from_address(addr),
    ///         Default::default(),
    ///         ClientRequest::builder()
    ///             .method(Method::GET)
    ///             .uri("/")
    ///             .body(Body::empty())
    ///             .expect("build request failed"),
    ///         None,
    ///     )
    ///     .await
    ///     .expect("request failed")
    ///     .into_string()
    ///     .await
    ///     .expect("response failed to convert to string");
    /// println!("{resp:?}");
    /// # })
    /// ```
    pub async fn send_request<B>(
        &self,
        target: Target,
        call_opt: Option<CallOpt>,
        mut request: ClientRequest<B>,
        timeout: Option<Duration>,
    ) -> Result<S::Response, S::Error>
    where
        S: Service<ClientContext, ClientRequest<B>, Response=ClientResponse, Error=ClientError>
        + Send
        + Sync
        + 'static,
        B: Send + 'static,
    {
        let caller_name = self.inner.caller_name.clone();
        let callee_name = self.inner.callee_name.clone();

        let (target, call_opt) = match (target.is_none(), self.inner.default_target.is_none()) {
            // The target specified by request exists and we can use it directly.
            //
            // Note that the default callopt only applies to the default target and should not be
            // used here.
            (false, _) => (target, call_opt.as_ref()),
            // Target is not specified by request, we can use the default target.
            //
            // Although the request does not set a target, its callopt should be valid for the
            // default target.
            (true, false) => (
                self.inner.default_target.clone(),
                call_opt.as_ref().or(self.inner.default_call_opt.as_ref()),
            ),
            // Both target are none, return an error.
            (true, true) => {
                return Err(no_address());
            }
        };

        let host = if callee_name.is_empty() {
            target.gen_host()
        } else {
            HeaderValue::from_str(callee_name.as_str()).ok()
        };
        if let Some(host) = host {
            request.headers_mut().insert(header::HOST, host);
        }

        let mut cx = ClientContext::new();
        cx.rpc_info_mut().caller_mut().set_service_name(caller_name);
        cx.rpc_info_mut().callee_mut().set_service_name(callee_name);
        (self.inner.target_parser)(target, call_opt, cx.rpc_info_mut().callee_mut());

        let config = cx.rpc_info_mut().config_mut();
        config.clone_from(&self.inner.default_config);
        config.timeout = timeout.or(config.timeout);

        self.call(&mut cx, request).await
    }
}

impl<S, B> Service<ClientContext, ClientRequest<B>> for Client<S>
where
    S: Service<ClientContext, ClientRequest<B>, Response=ClientResponse, Error=ClientError>
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

        let fut = self.inner.service.call(cx, req);

        if has_metainfo {
            fut.await
        } else {
            METAINFO.scope(RefCell::new(MetaInfo::default()), fut).await
        }
    }
}

/// A dummy [`MkClient`] that does not have any functionality
pub struct DefaultMkClient;

impl<S> MkClient<Client<S>> for DefaultMkClient {
    type Target = Client<S>;

    fn mk_client(&self, service: Client<S>) -> Self::Target {
        service
    }
}

/// Create a GET request to the specified URI.
pub async fn get<U>(uri: U) -> Result<ClientResponse>
where
    U: TryInto<Uri>,
    U::Error: Into<BoxError>,
{
    ClientBuilder::new().build().get(uri).send().await
}

// The `httpbin.org` always responses a json data.
#[cfg(feature = "json")]
#[cfg(test)]
mod client_tests {
    use std::{
        collections::HashMap,
        future::Future,
    };

    use http::{header, StatusCode};
    use motore::{
        layer::{Layer, Stack},
        service::Service,
    };
    use serde::Deserialize;
    use volo::{context::Endpoint, layer::Identity};

    use super::{
        callopt::CallOpt,
        dns::{parse_target, DnsResolver},
        get, Client, DefaultClient, Target,
    };
    use crate::{
        body::BodyConversion,
        client::cookie::CookieLayer,
        error::client::status_error,
        utils::consts::HTTP_DEFAULT_PORT,
        ClientBuilder,
    };
    use crate::response::ResponseExt;

    #[derive(Deserialize)]
    struct HttpBinResponse {
        args: HashMap<String, String>,
        headers: HashMap<String, String>,
        #[allow(unused)]
        origin: String,
        url: String,
    }

    const HTTPBIN_GET: &str = "http://httpbin.org/get";
    #[cfg(feature = "__tls")]
    const HTTPBIN_GET_HTTPS: &str = "https://httpbin.org/get";
    const USER_AGENT_KEY: &str = "User-Agent";
    const USER_AGENT_VAL: &str = "volo-http-unit-test";

    #[test]
    fn client_types_check() {
        struct TestLayer;
        struct TestService<S> {
            inner: S,
        }

        impl<S> Layer<S> for TestLayer {
            type Service = TestService<S>;

            fn layer(self, inner: S) -> Self::Service {
                TestService { inner }
            }
        }

        impl<S, Cx, Req> Service<Cx, Req> for TestService<S>
        where
            S: Service<Cx, Req>,
        {
            type Response = S::Response;
            type Error = S::Error;

            fn call(
                &self,
                cx: &mut Cx,
                req: Req,
            ) -> impl Future<Output=Result<Self::Response, Self::Error>> + Send {
                self.inner.call(cx, req)
            }
        }

        let _: DefaultClient = ClientBuilder::new().build();
        let _: DefaultClient<TestLayer> = ClientBuilder::new().layer_inner(TestLayer).build();
        let _: DefaultClient<TestLayer> = ClientBuilder::new().layer_inner_front(TestLayer).build();
        let _: DefaultClient<Identity, TestLayer> =
            ClientBuilder::new().layer_outer(TestLayer).build();
        let _: DefaultClient<Identity, TestLayer> =
            ClientBuilder::new().layer_outer_front(TestLayer).build();
        let _: DefaultClient<TestLayer, TestLayer> = ClientBuilder::new()
            .layer_inner(TestLayer)
            .layer_outer(TestLayer)
            .build();
        let _: DefaultClient<Stack<TestLayer, TestLayer>> = ClientBuilder::new()
            .layer_inner(TestLayer)
            .layer_inner(TestLayer)
            .build();
    }

    #[tokio::test]
    async fn simple_get() {
        let resp = get(HTTPBIN_GET)
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert!(resp.args.is_empty());
        assert_eq!(resp.url, HTTPBIN_GET);
    }

    #[tokio::test]
    async fn client_builder_with_header() {
        let mut builder = Client::builder();
        builder.header(header::USER_AGENT, USER_AGENT_VAL).unwrap();
        let client = builder.build();

        let resp = client
            .get(HTTPBIN_GET)
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert!(resp.args.is_empty());
        assert_eq!(resp.headers.get(USER_AGENT_KEY).unwrap(), USER_AGENT_VAL);
        assert_eq!(resp.url, HTTPBIN_GET);
    }

    #[tokio::test]
    async fn client_builder_with_host() {
        let mut builder = Client::builder();
        builder.host("httpbin.org");
        let client = builder.build();

        let resp = client
            .get("/get")
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert!(resp.args.is_empty());
        assert_eq!(resp.url, HTTPBIN_GET);
    }

    #[tokio::test]
    async fn client_builder_with_address() {
        let addr = DnsResolver::default()
            .resolve("httpbin.org", HTTP_DEFAULT_PORT)
            .await
            .unwrap();
        let mut builder = Client::builder();
        builder.address(addr).callee_name("httpbin.org");
        let client = builder.build();

        let resp = client
            .get("/get")
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert!(resp.args.is_empty());
        assert_eq!(resp.url, HTTPBIN_GET);
    }

    #[cfg(feature = "__tls")]
    #[tokio::test]
    async fn client_builder_with_https() {
        let mut builder = Client::builder();
        builder.host("httpbin.org").with_https(true);
        let client = builder.build();

        let resp = client
            .get("/get")
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert!(resp.args.is_empty());
        assert_eq!(resp.url, HTTPBIN_GET_HTTPS);
    }

    #[cfg(feature = "__tls")]
    #[tokio::test]
    async fn client_builder_with_address_and_https() {
        let addr = DnsResolver::default()
            .resolve("httpbin.org", crate::utils::consts::HTTPS_DEFAULT_PORT)
            .await
            .unwrap();
        let mut builder = Client::builder();
        builder
            .address(addr)
            .with_https(true)
            .callee_name("httpbin.org");
        let client = builder.build();

        let resp = client
            .get("/get")
            .send()
            .await
            .unwrap()
            .into_json::<HttpBinResponse>()
            .await
            .unwrap();
        assert!(resp.args.is_empty());
        assert_eq!(resp.url, HTTPBIN_GET_HTTPS);
    }

    #[tokio::test]
    async fn client_builder_with_port() {
        let mut builder = Client::builder();
        builder.host("httpbin.org").with_port(443);
        let client = builder.build();

        let resp = client.get("/get").send().await.unwrap();
        // Send HTTP request to the HTTPS port (443), `httpbin.org` will response `400 Bad
        // Request`.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn fail_on_status() {
        let mut builder = Client::builder();
        builder.host("httpbin.org").fail_on_error_status(true);
        let client = builder.build();
        assert_eq!(
            format!(
                "{}",
                client
                    .get("/post")
                    .send()
                    .await
                    .expect_err("GET for httpbin.org/post should fail")
            ),
            format!("{}", status_error(StatusCode::METHOD_NOT_ALLOWED)),
        );
    }

    #[cfg(feature = "__tls")]
    #[tokio::test]
    async fn client_disable_tls() {
        use crate::error::client::bad_scheme;

        let mut builder = Client::builder();
        builder.disable_tls(true);
        let client = builder.build();
        assert_eq!(
            format!(
                "{}",
                client
                    .get("https://httpbin.org/get")
                    .send()
                    .await
                    .expect_err("HTTPS with disable_tls should fail")
            ),
            format!("{}", bad_scheme()),
        );
    }

    struct CallOptInserted;

    // Wrapper for [`parse_target`] with checking [`CallOptInserted`]
    fn callopt_should_inserted(
        target: Target,
        call_opt: Option<&CallOpt>,
        endpoint: &mut Endpoint,
    ) {
        assert!(call_opt.is_some());
        assert!(call_opt.unwrap().contains::<CallOptInserted>());
        parse_target(target, call_opt, endpoint);
    }

    fn callopt_should_not_inserted(
        target: Target,
        call_opt: Option<&CallOpt>,
        endpoint: &mut Endpoint,
    ) {
        if let Some(call_opt) = call_opt {
            assert!(!call_opt.contains::<CallOptInserted>());
        }
        parse_target(target, call_opt, endpoint);
    }

    #[tokio::test]
    async fn no_callopt() {
        let mut builder = Client::builder();
        builder.target_parser(callopt_should_not_inserted);
        let client = builder.build();

        let resp = client.get(HTTPBIN_GET).send().await;
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn default_callopt() {
        let mut builder = Client::builder();
        builder.with_callopt(CallOpt::new().with(CallOptInserted));
        builder.target_parser(callopt_should_not_inserted);
        let client = builder.build();

        let resp = client.get(HTTPBIN_GET).send().await;
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn request_callopt() {
        let mut builder = Client::builder();
        builder.target_parser(callopt_should_inserted);
        let client = builder.build();

        let resp = client
            .get(HTTPBIN_GET)
            .with_callopt(CallOpt::new().with(CallOptInserted))
            .send()
            .await;
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn override_callopt() {
        let mut builder = Client::builder();
        builder.with_callopt(CallOpt::new().with(CallOptInserted));
        builder.target_parser(callopt_should_not_inserted);
        let client = builder.build();

        let resp = client
            .get(HTTPBIN_GET)
            // insert an empty callopt
            .with_callopt(CallOpt::new())
            .send()
            .await;
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn default_target_and_callopt_with_new_target() {
        let mut builder = Client::builder();
        builder.host("httpbin.org");
        builder.with_callopt(CallOpt::new().with(CallOptInserted));
        builder.target_parser(callopt_should_not_inserted);
        let client = builder.build();

        let resp = client.get(HTTPBIN_GET).send().await;
        assert!(resp.is_ok());
    }

    #[tokio::test]
    async fn cookie_store() {
        let mut builder = Client::builder()
            .layer_inner(CookieLayer::new(Default::default()));

        builder.host("httpbin.org");

        let client = builder.build();

        // test server add cookie
        let resp = client.get("http://httpbin.org/cookies/set?key=value").send().await.unwrap();
        let cookies = resp.cookies().collect::<Vec<_>>();
        assert_eq!(cookies[0].name(), "key");
        assert_eq!(cookies[0].value(), "value");

        // test server delete cookie
        _ = client.get("http://httpbin.org/cookies/delete?key").send().await.unwrap();
        let resp = client.get(HTTPBIN_GET).send().await.unwrap();
        let cookies = resp.cookies().collect::<Vec<_>>();
        assert_eq!(cookies.len(), 0)
    }
}
