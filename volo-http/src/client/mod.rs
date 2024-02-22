use std::{cell::RefCell, error::Error, sync::Arc};

use faststr::FastStr;
use http::{
    header::{self, HeaderMap, HeaderName, HeaderValue},
    Method,
};
use metainfo::{MetaInfo, METAINFO};
use motore::{
    layer::{Identity, Layer, Stack},
    make::MakeConnection,
    service::Service,
};
use paste::paste;
use volo::{
    client::MkClient,
    context::Context,
    net::{dial::DefaultMakeTransport, Address},
};

use self::{
    meta::MetaService,
    request_builder::RequestBuilder,
    transport::{ClientConfig, ClientTransport},
    utils::IntoUri,
};
use crate::{
    context::{
        client::{Config, Host, UserAgent},
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

pub type ClientMetaService<MkT> = MetaService<ClientTransport<MkT>>;

pub struct ClientBuilder<L, MkC, MkT> {
    config: Config,
    http_config: ClientConfig,
    callee_name: FastStr,
    caller_name: FastStr,
    headers: HeaderMap,
    user_agent: UserAgent,
    layer: L,
    mk_client: MkC,
    mk_conn: MkT,
}

impl ClientBuilder<Identity, DefaultMkClient, DefaultMakeTransport> {
    /// Create a new client builder.
    pub fn new() -> Self {
        Self {
            config: Default::default(),
            http_config: Default::default(),
            callee_name: FastStr::empty(),
            caller_name: FastStr::empty(),
            headers: Default::default(),
            user_agent: Default::default(),
            layer: Identity::new(),
            mk_client: DefaultMkClient,
            mk_conn: DefaultMakeTransport::new(),
        }
    }
}

impl Default for ClientBuilder<Identity, DefaultMkClient, DefaultMakeTransport> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L, MkC, MkT> ClientBuilder<L, MkC, MkT> {
    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn client_maker<MkC2>(self, new_mk_client: MkC2) -> ClientBuilder<L, MkC2, MkT> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            user_agent: self.user_agent,
            layer: self.layer,
            mk_client: new_mk_client,
            mk_conn: self.mk_conn,
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
    pub fn layer<Inner>(self, layer: Inner) -> ClientBuilder<Stack<Inner, L>, MkC, MkT> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            user_agent: self.user_agent,
            layer: Stack::new(layer, self.layer),
            mk_client: self.mk_client,
            mk_conn: self.mk_conn,
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
    pub fn layer_front<Front>(self, layer: Front) -> ClientBuilder<Stack<L, Front>, MkC, MkT> {
        ClientBuilder {
            config: self.config,
            http_config: self.http_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            user_agent: self.user_agent,
            layer: Stack::new(self.layer, layer),
            mk_client: self.mk_client,
            mk_conn: self.mk_conn,
        }
    }

    /// Sets the target server's name.
    pub fn callee_name<S>(mut self, callee: S) -> Self
    where
        S: Into<FastStr>,
    {
        self.callee_name = callee.into();
        self
    }

    /// Sets the client's name sent to the server.
    pub fn caller_name<S>(mut self, caller: S) -> Self
    where
        S: Into<FastStr>,
    {
        self.caller_name = caller.into();
        self
    }

    /// Insert a header to the request.
    pub fn header<K, V>(mut self, key: K, value: V) -> Result<Self, ClientError>
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

    /// Set mode for setting `User-Agent` in request headers.
    ///
    /// Default is generated crate name with version, e.g., `volo-http/0.1.0`
    pub fn set_user_agent(&mut self, ua: UserAgent) -> &mut Self {
        self.user_agent = ua;
        self
    }

    /// Set mode for setting `Host` in request headers.
    ///
    /// Default is callee name.
    pub fn set_host(&mut self, host: Host) -> &mut Self {
        self.config.host = host;
        self
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn stat_enable(&mut self, enable: bool) -> &mut Self {
        self.config.stat_enable = enable;
        self
    }

    /// Build the HTTP client.
    pub fn build(mut self) -> MkC::Target
    where
        L: Layer<MetaService<ClientTransport<MkT>>>,
        L::Service: Send + Sync + 'static,
        MkC: MkClient<Client<L::Service>>,
        MkT: MakeConnection<Address>,
    {
        let transport = ClientTransport::new(self.http_config, self.mk_conn);
        let service = self.layer.layer(MetaService::new(transport));
        if self.headers.get(header::USER_AGENT).is_some() {
            self.user_agent = UserAgent::None;
        }
        match self.user_agent {
            UserAgent::PkgNameWithVersion => self.headers.insert(
                header::USER_AGENT,
                HeaderValue::from_static(PKG_NAME_WITH_VER),
            ),
            UserAgent::CallerNameWithVersion if !self.caller_name.is_empty() => {
                self.headers.insert(
                    header::USER_AGENT,
                    HeaderValue::from_str(&format!(
                        "{}/{}",
                        self.caller_name,
                        env!("CARGO_PKG_VERSION")
                    ))
                    .expect("Invalid caller name"),
                )
            }
            UserAgent::Specified(val) if !val.is_empty() => self.headers.insert(
                header::USER_AGENT,
                val.try_into().expect("Invalid value for User-Agent"),
            ),
            _ => None,
        };
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

impl<S> Client<S> {
    /// Create a builder for building a request.
    pub fn builder(&self) -> RequestBuilder<S> {
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
    pub async fn send_request<B>(
        &self,
        host: Option<&str>,
        target: Address,
        request: ClientRequest<B>,
    ) -> Result<S::Response, S::Error>
    where
        S: Service<ClientContext, ClientRequest<B>, Response = ClientResponse, Error = ClientError>
            + Send
            + Sync
            + 'static,
        B: Send + 'static,
    {
        let caller_name = self.inner.caller_name.clone();
        let callee_name = if !self.inner.callee_name.is_empty() {
            self.inner.callee_name.clone()
        } else {
            match host {
                Some(host) => FastStr::from(host.to_owned()),
                None => FastStr::empty(),
            }
        };
        let mut cx = ClientContext::new(target, true);
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
