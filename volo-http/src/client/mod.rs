//! Client implementation
//!
//! See [`Client`] for more details.

use std::{
    borrow::Cow,
    cell::RefCell,
    error::Error,
    future::Future,
    sync::{Arc, LazyLock},
    time::Duration,
};

use faststr::FastStr;
use http::{
    header::{HeaderMap, HeaderName, HeaderValue},
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
    client::{Apply, MkClient, OneShotService},
    context::Context,
    loadbalance::MkLbLayer,
    net::{
        dial::{DefaultMakeTransport, MakeTransport},
        Address,
    },
};

use self::{
    layer::{
        header::{Host, HostService, UserAgent, UserAgentService},
        Timeout,
    },
    loadbalance::{DefaultLB, LbConfig},
    transport::{ClientConfig, ClientTransport, ClientTransportConfig},
};
use crate::{
    context::ClientContext,
    error::{
        client::{builder_error, Result},
        BoxError, ClientError,
    },
    request::ClientRequest,
    response::ClientResponse,
};

mod callopt;
#[cfg(feature = "cookie")]
pub mod cookie;
pub mod dns;
pub mod layer;
pub mod loadbalance;
mod request_builder;
pub mod target;
#[cfg(test)]
pub mod test_helpers;
mod transport;

pub use self::{callopt::CallOpt, request_builder::RequestBuilder, target::Target};

#[doc(hidden)]
pub mod prelude {
    pub use super::{Client, ClientBuilder};
}

/// Default inner service of [`Client`]
pub type ClientMetaService = ClientTransport;
/// [`Client`] generated service with given `IL`, `OL` and `LB`
pub type ClientService<IL = Identity, OL = Identity, LB = DefaultLB> = <OL as Layer<
    <<LB as MkLbLayer>::Layer as Layer<<IL as Layer<ClientMetaService>>::Service>>::Service,
>>::Service;
/// Default [`Client`] without default [`Layer`]s
pub type SimpleClient<IL = Identity, OL = Identity> = Client<ClientService<IL, OL>>;
/// Default [`Layer`]s that [`ClientBuilder::build`] append to outer layers
pub type DefaultClientOuterService<S> =
    <Timeout as Layer<HostService<UserAgentService<S>>>>::Service;
/// Default [`Client`] with default [`Layer`]s
pub type DefaultClient<IL = Identity, OL = Identity> =
    Client<DefaultClientOuterService<ClientService<IL, OL>>>;

/// A builder for configuring an HTTP [`Client`].
pub struct ClientBuilder<IL, OL, C, LB> {
    http_config: ClientConfig,
    builder_config: BuilderConfig,
    connector: DefaultMakeTransport,
    target: Target,
    timeout: Option<Duration>,
    user_agent: Option<HeaderValue>,
    host: Option<HeaderValue>,
    callee_name: FastStr,
    headers: HeaderMap,
    inner_layer: IL,
    outer_layer: OL,
    mk_client: C,
    mk_lb: LB,
    status: Result<()>,
    #[cfg(feature = "__tls")]
    tls_config: Option<volo::net::tls::TlsConnector>,
}

/// Configuration for [`ClientBuilder`]
///
/// This is unstable now and may be changed in the future.
#[doc(hidden)]
pub struct BuilderConfig {
    pub stat_enable: bool,
    #[cfg(feature = "__tls")]
    pub disable_tls: bool,
}

impl Default for BuilderConfig {
    fn default() -> Self {
        Self {
            stat_enable: true,
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
            target: Default::default(),
            timeout: None,
            user_agent: None,
            host: None,
            callee_name: FastStr::empty(),
            headers: Default::default(),
            inner_layer: Identity::new(),
            outer_layer: Identity::new(),
            mk_client: DefaultMkClient,
            mk_lb: Default::default(),
            status: Ok(()),
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.load_balance(load_balance),
            status: self.status,
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb.discover(discover),
            status: self.status,
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: new_mk_client,
            mk_lb: self.mk_lb,
            status: self.status,
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: Stack::new(layer, self.inner_layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            status: self.status,
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: Stack::new(self.inner_layer, layer),
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            status: self.status,
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(layer, self.outer_layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            status: self.status,
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: Stack::new(self.outer_layer, layer),
            mk_client: self.mk_client,
            mk_lb: self.mk_lb,
            status: self.status,
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
            target: self.target,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host: self.host,
            callee_name: self.callee_name,
            headers: self.headers,
            inner_layer: self.inner_layer,
            outer_layer: self.outer_layer,
            mk_client: self.mk_client,
            mk_lb: mk_load_balance,
            status: self.status,
            #[cfg(feature = "__tls")]
            tls_config: self.tls_config,
        }
    }

    /// Set default target address of the client.
    ///
    /// For using given `Host` in header or using HTTPS, use [`ClientBuilder::default_host`] for
    /// setting it.
    ///
    /// If there is no target specified when building a request, client will use this address.
    pub fn address<A>(&mut self, address: A) -> &mut Self
    where
        A: Into<Address>,
    {
        self.target = Target::from(address.into());
        self
    }

    /// Set default target host of the client.
    ///
    /// If there is no target specified when building a request, client will use this address.
    ///
    /// It uses http with port 80 by default.
    ///
    /// For setting scheme and port, use [`ClientBuilder::with_scheme`] and
    /// [`ClientBuilder::with_port`] after specifying host.
    pub fn host<S>(&mut self, host: S) -> &mut Self
    where
        S: Into<Cow<'static, str>>,
    {
        let host = host.into();
        self.target = Target::from_host(host.clone());
        self.default_host(host);
        self
    }

    /// Set port of the default target.
    ///
    /// If there is no target specified, the function will do nothing.
    pub fn with_port(&mut self, port: u16) -> &mut Self {
        if self.status.is_err() {
            return self;
        }

        if let Err(err) = self.target.set_port(port) {
            self.status = Err(err);
        }

        self
    }

    /// Set scheme of default target.
    pub fn with_scheme(&mut self, scheme: Scheme) -> &mut Self {
        if self.status.is_err() {
            return self;
        }

        if let Err(err) = self.target.set_scheme(scheme) {
            self.status = Err(err);
        }

        self
    }

    /// Insert a header to the request.
    pub fn header<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: TryInto<HeaderName>,
        K::Error: Error + Send + Sync + 'static,
        V: TryInto<HeaderValue>,
        V::Error: Error + Send + Sync + 'static,
    {
        if self.status.is_err() {
            return self;
        }

        if let Err(err) = insert_header(&mut self.headers, key, value) {
            self.status = Err(err);
        }
        self
    }

    /// Get a reference of [`Target`].
    pub fn target_ref(&self) -> &Target {
        &self.target
    }

    /// Get a mutable reference of [`Target`].
    pub fn target_mut(&mut self) -> &mut Target {
        &mut self.target
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

    /// Set the maximum idle time for the whole request.
    pub fn set_request_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set default `User-Agent` in request header.
    ///
    /// If there is `User-Agent` given, a default `User-Agent` will be generated by crate name and
    /// version.
    pub fn user_agent<V>(&mut self, val: V) -> &mut Self
    where
        V: TryInto<HeaderValue>,
        V::Error: Error + Send + Sync + 'static,
    {
        if self.status.is_err() {
            return self;
        }
        match val.try_into() {
            Ok(val) => self.user_agent = Some(val),
            Err(err) => self.status = Err(builder_error(err)),
        }
        self
    }

    /// Set default `Host` for service name and request header.
    ///
    /// If there is no default `Host`, it will be generated by target hostname and address for each
    /// request.
    pub fn default_host<S>(&mut self, host: S) -> &mut Self
    where
        S: Into<Cow<'static, str>>,
    {
        if self.status.is_err() {
            return self;
        }
        let host = FastStr::from(host.into());
        match HeaderValue::from_str(&host) {
            Ok(val) => self.host = Some(val),
            Err(err) => self.status = Err(builder_error(err)),
        }
        self.callee_name = host;
        self
    }

    /// Build the HTTP client with default configurations.
    ///
    /// This method will insert some default layers: [`Timeout`], [`UserAgent`] and [`Host`], and
    /// the final calling sequence will be as follows:
    ///
    /// - Outer:
    ///   - [`Timeout`]: Apply timeout from [`ClientBuilder::set_request_timeout`] or
    ///     [`CallOpt::with_timeout`]. Note that without this layer, timeout from [`Client`] or
    ///     [`CallOpt`] will not work.
    ///   - [`Host`]: Insert `Host` to request headers, it takes the given value from
    ///     [`ClientBuilder::default_host`] or generating by request everytime. If there is already
    ///     a `Host`, the layer does nothing.
    ///   - [`UserAgent`]: Insert `User-Agent` into the request header, it takes the given value
    ///     from [`ClientBuilder::user_agent`] or generates a value based on the current package
    ///     name and version. If `User-Agent` already exists, this layer does nothing.
    ///   - Other outer layers
    /// - LoadBalance ([`LbConfig`] with [`DnsResolver`] by default)
    /// - Inner layers
    ///   - Other inner layers
    /// - Transport through network or unix domain socket.
    ///
    /// [`DnsResolver`]: crate::client::dns::DnsResolver
    pub fn build(mut self) -> Result<C::Target>
    where
        IL: Layer<ClientMetaService>,
        IL::Service: Send + Sync + 'static,
        LB: MkLbLayer,
        LB::Layer: Layer<IL::Service>,
        <LB::Layer as Layer<IL::Service>>::Service: Send + Sync,
        OL: Layer<<LB::Layer as Layer<IL::Service>>::Service>,
        OL::Service: Send + Sync + 'static,
        C: MkClient<
            Client<<Timeout as Layer<HostService<UserAgentService<OL::Service>>>>::Service>,
        >,
    {
        let timeout_layer = Timeout;
        let host_layer = match self.host.take() {
            Some(host) => Host::new(host),
            None => Host::auto(),
        };
        let ua_layer = match self.user_agent.take() {
            Some(ua) => UserAgent::new(ua),
            None => UserAgent::auto(),
        };
        self.layer_outer_front(ua_layer)
            .layer_outer_front(host_layer)
            .layer_outer_front(timeout_layer)
            .build_without_extra_layers()
    }

    /// Build the HTTP client without inserting any extra layers.
    ///
    /// This method is provided for advanced users, some features may not work properly without the
    /// default layers,
    ///
    /// See [`ClientBuilder::build`] for more details.
    pub fn build_without_extra_layers(self) -> Result<C::Target>
    where
        IL: Layer<ClientTransport>,
        IL::Service: Send + Sync + 'static,
        LB: MkLbLayer,
        LB::Layer: Layer<IL::Service>,
        <LB::Layer as Layer<IL::Service>>::Service: Send + Sync,
        OL: Layer<<LB::Layer as Layer<IL::Service>>::Service>,
        OL::Service: Send + Sync + 'static,
        C: MkClient<Client<OL::Service>>,
    {
        self.status?;

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
        let service = self
            .outer_layer
            .layer(self.mk_lb.make().layer(self.inner_layer.layer(transport)));

        let client_inner = ClientInner {
            service,
            target: self.target,
            timeout: self.timeout,
            default_callee_name: self.callee_name,
            headers: self.headers,
        };
        let client = Client {
            inner: Arc::new(client_inner),
        };
        Ok(self.mk_client.mk_client(client))
    }
}

fn insert_header<K, V>(headers: &mut HeaderMap, key: K, value: V) -> Result<()>
where
    K: TryInto<HeaderName>,
    K::Error: Error + Send + Sync + 'static,
    V: TryInto<HeaderValue>,
    V::Error: Error + Send + Sync + 'static,
{
    headers.insert(
        key.try_into().map_err(builder_error)?,
        value.try_into().map_err(builder_error)?,
    );
    Ok(())
}

struct ClientInner<S> {
    service: S,
    target: Target,
    timeout: Option<Duration>,
    default_callee_name: FastStr,
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
/// let client = Client::builder().build().unwrap();
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

impl Default for DefaultClient {
    fn default() -> Self {
        ClientBuilder::default().build().unwrap()
    }
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
            pub fn [<$method:lower>]<U>(&self, uri: U) -> RequestBuilder<Self>
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
    pub fn request_builder(&self) -> RequestBuilder<Self> {
        RequestBuilder::new(self.clone())
    }

    /// Create a builder for building a request with the specified method and URI.
    pub fn request<U>(&self, method: Method, uri: U) -> RequestBuilder<Self>
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
        &self.inner.target
    }
}

impl<S, B> OneShotService<ClientContext, ClientRequest<B>> for Client<S>
where
    S: Service<ClientContext, ClientRequest<B>, Error = ClientError> + Send + Sync,
    B: Send,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        self,
        cx: &mut ClientContext,
        mut req: ClientRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        // set target
        self.inner.target.clone().apply(cx)?;

        // also save a scheme in request
        {
            if let Some(scheme) = cx.rpc_info().callee().get::<Scheme>() {
                req.extensions_mut().insert(scheme.to_owned());
            }
        }

        // set default callee name
        {
            let callee = cx.rpc_info_mut().callee_mut();
            if callee.service_name_ref().is_empty() {
                callee.set_service_name(self.inner.default_callee_name.clone());
            }
        }

        // set timeout
        {
            let config = cx.rpc_info_mut().config_mut();
            // We should check it here because CallOptService must be outer of the client service
            if config.timeout().is_none() {
                config.set_timeout(self.inner.timeout);
            }
        }

        // extend headermap
        req.headers_mut().extend(self.inner.headers.clone());

        // apply metainfo if it does not exist
        let has_metainfo = METAINFO.try_with(|_| {}).is_ok();

        let fut = self.inner.service.call(cx, req);

        if has_metainfo {
            fut.await
        } else {
            METAINFO.scope(RefCell::new(MetaInfo::default()), fut).await
        }
    }
}

impl<S, B> Service<ClientContext, ClientRequest<B>> for Client<S>
where
    S: Service<ClientContext, ClientRequest<B>, Error = ClientError> + Send + Sync,
    B: Send,
{
    type Response = S::Response;
    type Error = S::Error;

    fn call(
        &self,
        cx: &mut ClientContext,
        req: ClientRequest<B>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        OneShotService::call(self.clone(), cx, req)
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

static CLIENT: LazyLock<DefaultClient> = LazyLock::new(Default::default);

/// Create a GET request to the specified URI.
pub async fn get<U>(uri: U) -> Result<ClientResponse>
where
    U: TryInto<Uri>,
    U::Error: Into<BoxError>,
{
    CLIENT.clone().get(uri).send().await
}

// The `httpbin.org` always responses a json data.
#[cfg(feature = "json")]
#[cfg(test)]
mod client_tests {
    use std::{collections::HashMap, future::Future};

    #[cfg(feature = "cookie")]
    use cookie::Cookie;
    use http::{header, status::StatusCode};
    use motore::{
        layer::{Identity, Layer, Stack},
        service::Service,
    };
    use serde::Deserialize;

    use super::{dns::DnsResolver, get, Client, DefaultClient};
    #[cfg(feature = "cookie")]
    use crate::client::cookie::CookieLayer;
    use crate::{
        body::BodyConversion, client::SimpleClient, utils::consts::HTTP_DEFAULT_PORT, ClientBuilder,
    };

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
            ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
                self.inner.call(cx, req)
            }
        }

        let _: SimpleClient = ClientBuilder::new().build_without_extra_layers().unwrap();
        let _: SimpleClient<TestLayer> = ClientBuilder::new()
            .layer_inner(TestLayer)
            .build_without_extra_layers()
            .unwrap();
        let _: SimpleClient<Identity, TestLayer> = ClientBuilder::new()
            .layer_outer(TestLayer)
            .build_without_extra_layers()
            .unwrap();
        let _: SimpleClient<TestLayer, TestLayer> = ClientBuilder::new()
            .layer_inner(TestLayer)
            .layer_outer(TestLayer)
            .build_without_extra_layers()
            .unwrap();

        let _: DefaultClient = ClientBuilder::new().build().unwrap();
        let _: DefaultClient<TestLayer> =
            ClientBuilder::new().layer_inner(TestLayer).build().unwrap();
        let _: DefaultClient<TestLayer> = ClientBuilder::new()
            .layer_inner_front(TestLayer)
            .build()
            .unwrap();
        let _: DefaultClient<Identity, TestLayer> =
            ClientBuilder::new().layer_outer(TestLayer).build().unwrap();
        let _: DefaultClient<Identity, TestLayer> = ClientBuilder::new()
            .layer_outer_front(TestLayer)
            .build()
            .unwrap();
        let _: DefaultClient<TestLayer, TestLayer> = ClientBuilder::new()
            .layer_inner(TestLayer)
            .layer_outer(TestLayer)
            .build()
            .unwrap();
        let _: DefaultClient<Stack<TestLayer, TestLayer>> = ClientBuilder::new()
            .layer_inner(TestLayer)
            .layer_inner(TestLayer)
            .build()
            .unwrap();
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
        builder.header(header::USER_AGENT, USER_AGENT_VAL);
        let client = builder.build().unwrap();

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
        let client = builder.build().unwrap();

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
        builder.default_host("httpbin.org").address(addr);
        let client = builder.build().unwrap();

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
        builder
            .host("httpbin.org")
            .with_scheme(http::uri::Scheme::HTTPS);
        let client = builder.build().unwrap();

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
            .default_host("httpbin.org")
            .address(addr)
            .with_scheme(http::uri::Scheme::HTTPS);
        let client = builder.build().unwrap();

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
        let client = builder.build().unwrap();

        let resp = client.get("/get").send().await.unwrap();
        // Send HTTP request to the HTTPS port (443), `httpbin.org` will response `400 Bad
        // Request`.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[cfg(feature = "__tls")]
    #[tokio::test]
    async fn client_disable_tls() {
        use crate::error::client::bad_scheme;

        let mut builder = Client::builder();
        builder.disable_tls(true);
        let client = builder.build().unwrap();
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

    #[cfg(feature = "cookie")]
    #[tokio::test]
    async fn cookie_store() {
        let mut builder = Client::builder().layer_inner(CookieLayer::new(Default::default()));

        builder.host("httpbin.org");

        let client = builder.build().unwrap();

        // test server add cookie
        let resp = client
            .get("http://httpbin.org/cookies/set?key=value")
            .send()
            .await
            .unwrap();
        let cookies = resp
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| {
                std::str::from_utf8(value.as_bytes())
                    .ok()
                    .and_then(|val| Cookie::parse(val).map(|c| c.into_owned()).ok())
            })
            .collect::<Vec<_>>();
        assert_eq!(cookies[0].name(), "key");
        assert_eq!(cookies[0].value(), "value");

        #[derive(serde::Deserialize)]
        struct CookieResponse {
            #[serde(default)]
            cookies: HashMap<String, String>,
        }
        let resp = client
            .get("http://httpbin.org/cookies")
            .send()
            .await
            .unwrap();
        let json = resp.into_json::<CookieResponse>().await.unwrap();
        assert_eq!(json.cookies["key"], "value");

        // test server delete cookie
        _ = client
            .get("http://httpbin.org/cookies/delete?key")
            .send()
            .await
            .unwrap();
        let resp = client
            .get("http://httpbin.org/cookies")
            .send()
            .await
            .unwrap();
        let json = resp.into_json::<CookieResponse>().await.unwrap();
        assert_eq!(json.cookies.len(), 0);
    }
}
