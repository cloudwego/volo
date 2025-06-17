//! Client implementation
//!
//! See [`Client`] for more details.

use std::{
    cell::RefCell,
    error::Error,
    future::Future,
    sync::{Arc, LazyLock},
    time::Duration,
};

use http::{
    header::{HeaderMap, HeaderName, HeaderValue},
    method::Method,
    uri::Uri,
};
use metainfo::{MetaInfo, METAINFO};
use motore::{
    layer::{Identity, Layer, Stack},
    service::{BoxService, Service},
};
use paste::paste;
use volo::{
    client::{MkClient, OneShotService},
    context::Context,
    loadbalance::MkLbLayer,
    net::dial::{DefaultMakeTransport, MakeTransport},
};

use self::{
    layer::{
        header::{Host, UserAgent},
        Timeout,
    },
    loadbalance::{DefaultLb, LbConfig},
    transport::{
        pool,
        protocol::{ClientConfig, ClientTransportConfig},
    },
};
use crate::{
    body::Body,
    context::ClientContext,
    error::{
        client::{builder_error, Result},
        BoxError, ClientError,
    },
    request::Request,
    response::Response,
};

mod callopt;
#[cfg(test)]
mod client_tests;
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
mod utils;

pub use self::{
    callopt::CallOpt, request_builder::RequestBuilder, target::Target,
    transport::protocol::ClientTransport,
};

#[doc(hidden)]
pub mod prelude {
    pub use super::{Client, ClientBuilder};
}

/// A builder for configuring an HTTP [`Client`].
pub struct ClientBuilder<IL, OL, C, LB> {
    http_config: ClientConfig,
    client_config: ClientTransportConfig,
    pool_config: pool::Config,
    connector: DefaultMakeTransport,
    timeout: Option<Duration>,
    user_agent: Option<HeaderValue>,
    host_mode: Host,
    headers: HeaderMap,
    inner_layer: IL,
    outer_layer: OL,
    mk_client: C,
    mk_lb: LB,
    status: Result<()>,
    #[cfg(feature = "__tls")]
    tls_config: Option<volo::net::tls::TlsConnector>,
}

impl ClientBuilder<Identity, Identity, DefaultMkClient, DefaultLb> {
    /// Create a new client builder.
    pub fn new() -> Self {
        Self {
            http_config: Default::default(),
            client_config: Default::default(),
            pool_config: pool::Config::default(),
            connector: Default::default(),
            timeout: None,
            user_agent: None,
            host_mode: Host::Auto,
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

impl Default for ClientBuilder<Identity, Identity, DefaultMkClient, DefaultLb> {
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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
            client_config: self.client_config,
            pool_config: self.pool_config,
            connector: self.connector,
            timeout: self.timeout,
            user_agent: self.user_agent,
            host_mode: self.host_mode,
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

    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Default is false.
    #[deprecated(
        since = "0.4.0",
        note = "`set_title_case_headers` has been removed into `http1_config`"
    )]
    #[cfg(feature = "http1")]
    pub fn set_title_case_headers(&mut self, title_case_headers: bool) -> &mut Self {
        self.http_config
            .h1
            .set_title_case_headers(title_case_headers);
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
    #[deprecated(
        since = "0.4.0",
        note = "`set_max_headers` has been removed into `http1_config`"
    )]
    #[cfg(feature = "http1")]
    pub fn set_max_headers(&mut self, max_headers: usize) -> &mut Self {
        self.http_config.h1.set_max_headers(max_headers);
        self
    }

    /// Get configuration of http1 part.
    #[cfg(feature = "http1")]
    pub fn http1_config(&mut self) -> &mut self::transport::http1::Config {
        &mut self.http_config.h1
    }

    /// Get configuration of http2 part.
    #[cfg(feature = "http2")]
    pub fn http2_config(&mut self) -> &mut self::transport::http2::Config {
        &mut self.http_config.h2
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn stat_enable(&mut self, enable: bool) -> &mut Self {
        self.client_config.stat_enable = enable;
        self
    }

    /// Disable TLS for the client.
    ///
    /// Default is false, when TLS related feature is enabled, TLS is enabled by default.
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn disable_tls(&mut self, disable: bool) -> &mut Self {
        self.client_config.disable_tls = disable;
        self
    }

    /// Set idle timeout of connection pool.
    ///
    /// If a connection is idle for more than the timeout, the connection will be dropped.
    ///
    /// Default is 20 seconds.
    pub fn set_pool_idle_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.pool_config.idle_timeout = timeout;
        self
    }

    /// Set the maximum number of idle connections per host.
    ///
    /// If the number of idle connections on a host exceeds this value, the connection pool will
    /// refuse to add new idle connections.
    ///
    /// Default is 10240.
    pub fn set_max_idle_per_host(&mut self, num: usize) -> &mut Self {
        self.pool_config.max_idle_per_host = num;
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

    /// Set mode of client setting `Host` in headers.
    ///
    /// This mode only works when building client by [`ClientBuilder::build`],
    /// [`ClientBuilder::build_without_extra_layers`] will ignore this config.
    ///
    /// For more configurations, refer to [`Host`].
    ///
    /// Default is [`Host::Auto`], it will generate a `Host` by target domain name or address if
    /// there is no `Host` in request headers.
    pub fn host_mode(&mut self, mode: Host) -> &mut Self {
        self.host_mode = mode;
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
    ///   - [`Host`]: Insert `Host` to request headers. [`Host::Auto`] will be applied by default,
    ///     it will insert a `Host` generated from current [`Target`] if there is no `Host` in
    ///     headers.
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
    pub fn build<InnerReqBody, OuterReqBody, RespBody>(mut self) -> Result<C::Target>
    where
        IL: Layer<ClientTransport<InnerReqBody>>,
        IL::Service: Send + Sync + 'static,
        LB: MkLbLayer,
        LB::Layer: Layer<IL::Service>,
        <LB::Layer as Layer<IL::Service>>::Service: Send + Sync,
        OL: Layer<<LB::Layer as Layer<IL::Service>>::Service>,
        OL::Service: Service<
                ClientContext,
                Request<OuterReqBody>,
                Response = Response<RespBody>,
                Error = ClientError,
            > + Send
            + Sync
            + 'static,
        C: MkClient<Client<OuterReqBody, RespBody>>,
        InnerReqBody: Send,
        OuterReqBody: Send + 'static,
        RespBody: Send,
    {
        let timeout_layer = Timeout;
        let host_layer = self.host_mode.clone();
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
    pub fn build_without_extra_layers<InnerReqBody, OuterReqBody, RespBody>(
        self,
    ) -> Result<C::Target>
    where
        IL: Layer<ClientTransport<InnerReqBody>>,
        IL::Service: Send + Sync + 'static,
        LB: MkLbLayer,
        LB::Layer: Layer<IL::Service>,
        <LB::Layer as Layer<IL::Service>>::Service: Send + Sync,
        OL: Layer<<LB::Layer as Layer<IL::Service>>::Service>,
        OL::Service: Service<
                ClientContext,
                Request<OuterReqBody>,
                Response = Response<RespBody>,
                Error = ClientError,
            > + Send
            + Sync
            + 'static,
        C: MkClient<Client<OuterReqBody, RespBody>>,
        InnerReqBody: Send,
        OuterReqBody: Send + 'static,
        RespBody: Send,
    {
        self.status?;

        let transport = ClientTransport::new(
            self.http_config,
            self.client_config,
            self.pool_config,
            #[cfg(feature = "__tls")]
            self.tls_config,
        );
        let service = self
            .outer_layer
            .layer(self.mk_lb.make().layer(self.inner_layer.layer(transport)));
        let service = BoxService::new(service);

        let client_inner = ClientInner {
            service,
            timeout: self.timeout,
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

struct ClientInner<ReqBody, RespBody> {
    service: BoxService<ClientContext, Request<ReqBody>, Response<RespBody>, ClientError>,
    timeout: Option<Duration>,
    headers: HeaderMap,
}

/// An Client for sending HTTP requests and handling HTTP responses.
///
/// # Examples
///
/// ```no_run
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
pub struct Client<ReqBody = Body, RespBody = Body> {
    inner: Arc<ClientInner<ReqBody, RespBody>>,
}

impl Default for Client {
    fn default() -> Self {
        ClientBuilder::default().build().unwrap()
    }
}

impl<ReqBody, RespBody> Clone for Client<ReqBody, RespBody> {
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

impl Client {
    /// Create a new client builder.
    pub fn builder() -> ClientBuilder<Identity, Identity, DefaultMkClient, DefaultLb> {
        ClientBuilder::new()
    }
}

impl<ReqBody, RespBody> Client<ReqBody, RespBody> {
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
}

impl<ReqBody, RespBody> OneShotService<ClientContext, Request<ReqBody>>
    for Client<ReqBody, RespBody>
where
    ReqBody: Send,
{
    type Response = Response<RespBody>;
    type Error = ClientError;

    async fn call(
        self,
        cx: &mut ClientContext,
        mut req: Request<ReqBody>,
    ) -> Result<Self::Response, Self::Error> {
        #[cfg(feature = "__tls")]
        if cx.target().scheme() == Some(&http::uri::Scheme::HTTPS) {
            // save scheme in request
            req.extensions_mut().insert(http::uri::Scheme::HTTPS);
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

impl<ReqBody, RespBody> Service<ClientContext, Request<ReqBody>> for Client<ReqBody, RespBody>
where
    ReqBody: Send,
{
    type Response = Response<RespBody>;
    type Error = ClientError;

    fn call(
        &self,
        cx: &mut ClientContext,
        req: Request<ReqBody>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        OneShotService::call(self.clone(), cx, req)
    }
}

/// A dummy [`MkClient`] that does not have any functionality
pub struct DefaultMkClient;

impl<C> MkClient<C> for DefaultMkClient {
    type Target = C;

    fn mk_client(&self, service: C) -> Self::Target {
        service
    }
}

static CLIENT: LazyLock<Client> = LazyLock::new(Default::default);

/// Create a GET request to the specified URI.
pub async fn get<U>(uri: U) -> Result<Response>
where
    U: TryInto<Uri>,
    U::Error: Into<BoxError>,
{
    CLIENT.get(uri).send().await
}
