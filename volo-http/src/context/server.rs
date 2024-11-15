//! Context and its utilities of server

use std::{borrow::Cow, str::FromStr, sync::Arc};

use http::HeaderName;
use volo::{
    context::{Context, Reusable, Role, RpcCx, RpcInfo},
    net::Address,
    newtype_impl_context,
};

use crate::{
    server::param::PathParamsVec,
    utils::macros::{impl_deref_and_deref_mut, impl_getter},
};

/// RPC context of http server
#[derive(Debug)]
pub struct ServerContext(pub(crate) RpcCx<ServerCxInner, Config>);

impl ServerContext {
    /// Create a new [`ServerContext`] with the address of client
    pub fn new(peer: Address) -> Self {
        let mut cx = RpcCx::new(
            RpcInfo::<Config>::with_role(Role::Server),
            ServerCxInner::default(),
        );
        cx.rpc_info_mut().caller_mut().set_address(peer);
        Self(cx)
    }
}

impl_deref_and_deref_mut!(ServerContext, RpcCx<ServerCxInner, Config>, 0);

newtype_impl_context!(ServerContext, Config, 0);

/// Inner details of [`ServerContext`]
#[derive(Clone, Debug, Default)]
pub struct ServerCxInner {
    /// Path params from [`Uri`]
    ///
    /// See [`Router::route`] and [`PathParamsVec`], [`PathParamsMap`] or [`PathParams`] for more
    /// details.
    ///
    /// [`Uri`]: http::uri::Uri
    /// [`Router::route`]: crate::server::route::Router::route
    /// [`PathParamsMap`]: crate::server::param::PathParamsMap
    /// [`PathParams`]: crate::server::param::PathParams
    pub params: PathParamsVec,
}

impl ServerCxInner {
    impl_getter!(params, PathParamsVec);
}

/// Config for extract client ip
#[derive(Clone, Debug)]
pub struct ClientIPConfig {
    remote_ip_headers: Vec<HeaderName>,
    trusted_cidrs: Vec<ipnet::IpNet>,
}

impl Default for ClientIPConfig {
    fn default() -> Self {
        Self {
            remote_ip_headers: vec![
                HeaderName::from_static("X-Forwarded-For"),
                HeaderName::from_static("X-Real-IP"),
            ],
            trusted_cidrs: vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()],
        }
    }
}

impl ClientIPConfig {
    /// Create a new [`ClientIPConfig`] with default values
    ///
    /// default remote ip headers: `["X-Forwarded-For", "X-Real-IP"]`
    ///
    /// default trusted cidrs: `["0.0.0.0/0", "::/0"]`
    pub fn new() -> Self {
        Default::default()
    }

    /// Get Real Client IP by parsing the given headers.
    ///
    /// See [`ClientIP`](crate::server::extract::ClientIP) for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::context::server::ClientIPConfig;
    ///
    /// let client_ip_config =
    ///     ClientIPConfig::new().with_remote_ip_headers(vec!["X-Real-IP", "X-Forwarded-For"]);
    /// ```
    pub fn with_remote_ip_headers<I>(
        self,
        headers: I,
    ) -> Result<Self, http::header::InvalidHeaderName>
    where
        I: IntoIterator,
        I::Item: Into<Cow<'static, str>>,
    {
        let headers = headers.into_iter().map(Into::into).collect::<Vec<_>>();
        let mut remote_ip_headers = Vec::with_capacity(headers.len());
        for header_str in headers {
            let header_value = match header_str {
                Cow::Owned(s) => HeaderName::from_str(&s)?,
                Cow::Borrowed(s) => HeaderName::from_str(s)?,
            };
            remote_ip_headers.push(header_value);
        }

        Ok(Self {
            remote_ip_headers,
            trusted_cidrs: self.trusted_cidrs,
        })
    }

    pub(crate) fn remote_ip_headers(&self) -> &Vec<HeaderName> {
        &self.remote_ip_headers
    }

    /// Get Real Client IP if it is trusted, otherwise it will just return caller ip.
    ///
    /// See [`ClientIP`](crate::server::extract::ClientIP) for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::context::server::ClientIPConfig;
    ///
    /// let client_ip_config = ClientIPConfig::new()
    ///     .with_trusted_cidrs(vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()]);
    /// ```
    pub fn with_trusted_cidrs<H>(self, cidrs: H) -> Self
    where
        H: IntoIterator<Item = ipnet::IpNet>,
    {
        Self {
            remote_ip_headers: self.remote_ip_headers,
            trusted_cidrs: cidrs.into_iter().collect(),
        }
    }

    pub(crate) fn trusted_cidrs(&self) -> &Vec<ipnet::IpNet> {
        &self.trusted_cidrs
    }
}

/// Configuration of the request
///
/// It is empty currently
#[derive(Clone, Debug, Default)]
pub struct Config {
    #[cfg(feature = "__tls")]
    tls: bool,

    // client ip config
    client_ip_config: Arc<ClientIPConfig>,
}

impl Config {
    /// Return if the request is using TLS.
    #[cfg(feature = "__tls")]
    pub fn is_tls(&self) -> bool {
        self.tls
    }

    #[cfg(feature = "__tls")]
    pub(crate) fn set_tls(&mut self, tls: bool) {
        self.tls = tls;
    }

    pub(crate) fn set_client_ip_config(&mut self, config: ClientIPConfig) {
        self.client_ip_config = Arc::new(config);
    }

    pub(crate) fn client_ip_config(&self) -> Arc<ClientIPConfig> {
        self.client_ip_config.clone()
    }
}

impl Reusable for Config {
    fn clear(&mut self) {
        *self = Default::default();
    }
}
