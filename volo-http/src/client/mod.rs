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

pub struct ClientBuilder<L, MkC, MkT> {
    client_config: ClientConfig,
    rpc_config: Config,
    callee_name: FastStr,
    caller_name: FastStr,
    headers: HeaderMap,
    user_agent: UserAgent,
    layer: L,
    mk_client: MkC,
    mk_conn: MkT,
}

impl ClientBuilder<Identity, DefaultMkClient, DefaultMakeTransport> {
    pub fn new() -> Self {
        Self {
            client_config: Default::default(),
            rpc_config: Default::default(),
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
    pub fn client_maker<MkC2>(self, new_mk_client: MkC2) -> ClientBuilder<L, MkC2, MkT> {
        ClientBuilder {
            client_config: self.client_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            user_agent: self.user_agent,
            layer: self.layer,
            mk_client: new_mk_client,
            mk_conn: self.mk_conn,
        }
    }

    pub fn layer<Inner>(self, layer: Inner) -> ClientBuilder<Stack<Inner, L>, MkC, MkT> {
        ClientBuilder {
            client_config: self.client_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            user_agent: self.user_agent,
            layer: Stack::new(layer, self.layer),
            mk_client: self.mk_client,
            mk_conn: self.mk_conn,
        }
    }

    pub fn layer_front<Front>(self, layer: Front) -> ClientBuilder<Stack<L, Front>, MkC, MkT> {
        ClientBuilder {
            client_config: self.client_config,
            rpc_config: self.rpc_config,
            callee_name: self.callee_name,
            caller_name: self.caller_name,
            headers: self.headers,
            user_agent: self.user_agent,
            layer: Stack::new(self.layer, layer),
            mk_client: self.mk_client,
            mk_conn: self.mk_conn,
        }
    }

    pub fn callee_name<S>(mut self, callee: S) -> Self
    where
        S: Into<FastStr>,
    {
        self.callee_name = callee.into();
        self
    }

    pub fn caller_name<S>(mut self, caller: S) -> Self
    where
        S: Into<FastStr>,
    {
        self.caller_name = caller.into();
        self
    }

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

    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    pub fn set_user_agent(mut self, ua: UserAgent) -> Self {
        self.user_agent = ua;
        self
    }

    pub fn set_host(mut self, host: Host) -> Self {
        self.rpc_config.host = host;
        self
    }
}

impl<L, MkC, MkT> ClientBuilder<L, MkC, MkT>
where
    L: Layer<MetaService<ClientTransport<MkT>>>,
    L::Service: Service<ClientContext, ClientRequest, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync
        + 'static,
    MkC: MkClient<Client<L::Service>>,
    MkT: MakeConnection<Address>,
{
    pub fn build(mut self) -> MkC::Target {
        let transport = ClientTransport::new(self.client_config, self.mk_conn);
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
            rpc_config: self.rpc_config,
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
    rpc_config: Config,
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
    pub fn builder(&self) -> RequestBuilder<S> {
        RequestBuilder::new(self)
    }

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

impl<S> Client<S>
where
    S: Service<ClientContext, ClientRequest, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync
        + 'static,
{
    pub async fn send_request(
        &self,
        host: Option<&str>,
        target: Address,
        request: ClientRequest,
    ) -> Result<S::Response, S::Error> {
        let caller_name = self.inner.caller_name.clone();
        let callee_name = if !self.inner.callee_name.is_empty() {
            self.inner.callee_name.clone()
        } else {
            match host {
                Some(host) => FastStr::from(host.to_owned()),
                None => FastStr::empty(),
            }
        };
        let mut cx = ClientContext::new(target);
        cx.rpc_info_mut().caller_mut().set_service_name(caller_name);
        cx.rpc_info_mut().callee_mut().set_service_name(callee_name);
        cx.rpc_info_mut().set_config(self.inner.rpc_config.clone());
        self.call(&mut cx, request).await
    }
}

impl<S> Service<ClientContext, ClientRequest> for Client<S>
where
    S: Service<ClientContext, ClientRequest, Response = ClientResponse, Error = ClientError>
        + Send
        + Sync
        + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ClientContext,
        mut req: ClientRequest,
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

pub async fn get<U>(uri: U) -> Result<ClientResponse, ClientError>
where
    U: IntoUri,
{
    ClientBuilder::new().build().get(uri)?.send().await
}
