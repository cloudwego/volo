//! Utilities for extracting original client ip
//!
//! See [`ClientIp`] for more details.
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    str::FromStr,
};

use http::{HeaderMap, HeaderName};
use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use motore::{Service, layer::Layer};
use volo::{context::Context, net::Address};

use crate::{context::ServerContext, request::Request};

/// [`Layer`] for extracting client ip
///
/// See [`ClientIp`] for more details.
#[derive(Clone, Debug, Default)]
pub struct ClientIpLayer {
    config: ClientIpConfig,
}

impl ClientIpLayer {
    /// Create a new [`ClientIpLayer`] with default config
    pub fn new() -> Self {
        Default::default()
    }

    /// Create a new [`ClientIpLayer`] with the given [`ClientIpConfig`]
    pub fn with_config(self, config: ClientIpConfig) -> Self {
        Self { config }
    }
}

impl<S> Layer<S> for ClientIpLayer
where
    S: Send + Sync + 'static,
{
    type Service = ClientIpService<S>;

    fn layer(self, inner: S) -> Self::Service {
        ClientIpService {
            service: inner,
            config: self.config,
        }
    }
}

/// Config for extract client ip
#[derive(Clone, Debug)]
pub struct ClientIpConfig {
    remote_ip_headers: Vec<HeaderName>,
    trusted_cidrs: Vec<IpNet>,
}

impl Default for ClientIpConfig {
    fn default() -> Self {
        Self {
            remote_ip_headers: vec![
                HeaderName::from_static("x-real-ip"),
                HeaderName::from_static("x-forwarded-for"),
            ],
            trusted_cidrs: vec![
                IpNet::V4(Ipv4Net::new_assert(Ipv4Addr::new(0, 0, 0, 0), 0)),
                IpNet::V6(Ipv6Net::new_assert(
                    Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0),
                    0,
                )),
            ],
        }
    }
}

impl ClientIpConfig {
    /// Create a new [`ClientIpConfig`] with default values
    ///
    /// default remote ip headers: `["X-Real-IP", "X-Forwarded-For"]`
    ///
    /// default trusted cidrs: `["0.0.0.0/0", "::/0"]`
    pub fn new() -> Self {
        Default::default()
    }

    /// Get Real Client IP by parsing the given headers.
    ///
    /// See [`ClientIp`] for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::server::utils::client_ip::ClientIpConfig;
    ///
    /// let client_ip_config =
    ///     ClientIpConfig::new().with_remote_ip_headers(vec!["X-Real-IP", "X-Forwarded-For"]);
    /// ```
    pub fn with_remote_ip_headers<I>(
        self,
        headers: I,
    ) -> Result<Self, http::header::InvalidHeaderName>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let headers = headers.into_iter().collect::<Vec<_>>();
        let mut remote_ip_headers = Vec::with_capacity(headers.len());
        for header_str in headers {
            let header_value = HeaderName::from_str(header_str.as_ref())?;
            remote_ip_headers.push(header_value);
        }

        Ok(Self {
            remote_ip_headers,
            trusted_cidrs: self.trusted_cidrs,
        })
    }

    /// Get Real Client IP if it is trusted, otherwise it will just return caller ip.
    ///
    /// See [`ClientIp`] for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::server::utils::client_ip::ClientIpConfig;
    ///
    /// let client_ip_config = ClientIpConfig::new()
    ///     .with_trusted_cidrs(vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()]);
    /// ```
    pub fn with_trusted_cidrs<H>(self, cidrs: H) -> Self
    where
        H: IntoIterator<Item = IpNet>,
    {
        Self {
            remote_ip_headers: self.remote_ip_headers,
            trusted_cidrs: cidrs.into_iter().collect(),
        }
    }
}

/// Return original client IP Address
///
/// If you want to get client IP by retrieving specific headers, you can use
/// [`with_remote_ip_headers`](ClientIpConfig::with_remote_ip_headers) to set the
/// headers.
///
/// If you want to get client IP that is trusted with specific cidrs, you can use
/// [`with_trusted_cidrs`](ClientIpConfig::with_trusted_cidrs) to set the cidrs.
///
/// # Example
///
/// ## Default config
///
/// default remote ip headers: `["X-Real-IP", "X-Forwarded-For"]`
///
/// default trusted cidrs: `["0.0.0.0/0", "::/0"]`
///
/// ```rust
/// ///
/// use volo_http::server::utils::client_ip::ClientIp;
/// use volo_http::server::{
///     Server,
///     route::{Router, get},
///     utils::client_ip::{ClientIpConfig, ClientIpLayer},
/// };
///
/// async fn handler(ClientIp(client_ip): ClientIp) -> String {
///     client_ip.unwrap().to_string()
/// }
///
/// let router: Router = Router::new()
///     .route("/", get(handler))
///     .layer(ClientIpLayer::new());
/// ```
///
/// ## With custom config
///
/// ```rust
/// use http::HeaderMap;
/// use volo_http::{
///     context::ServerContext,
///     server::{
///         Server,
///         route::{Router, get},
///         utils::client_ip::{ClientIp, ClientIpConfig, ClientIpLayer},
///     },
/// };
///
/// async fn handler(ClientIp(client_ip): ClientIp) -> String {
///     client_ip.unwrap().to_string()
/// }
///
/// let router: Router = Router::new().route("/", get(handler)).layer(
///     ClientIpLayer::new().with_config(
///         ClientIpConfig::new()
///             .with_remote_ip_headers(vec!["x-real-ip", "x-forwarded-for"])
///             .unwrap()
///             .with_trusted_cidrs(vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()]),
///     ),
/// );
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

/// [`ClientIpLayer`] generated [`Service`]
///
/// See [`ClientIp`] for more details.
#[derive(Clone, Debug)]
pub struct ClientIpService<S> {
    service: S,
    config: ClientIpConfig,
}

impl<S> ClientIpService<S> {
    fn get_client_ip(&self, cx: &ServerContext, headers: &HeaderMap) -> ClientIp {
        let remote_ip = match &cx.rpc_info().caller().address {
            Some(Address::Ip(socket_addr)) => Some(socket_addr.ip()),
            #[cfg(target_family = "unix")]
            Some(Address::Unix(_)) => None,
            #[allow(unreachable_patterns)]
            Some(_) => unimplemented!("unsupported type of address"),
            None => return ClientIp(None),
        };

        if let Some(remote_ip) = &remote_ip {
            if !self
                .config
                .trusted_cidrs
                .iter()
                .any(|cidr| cidr.contains(remote_ip))
            {
                return ClientIp(None);
            }
        }

        for remote_ip_header in self.config.remote_ip_headers.iter() {
            let Some(remote_ips) = headers.get(remote_ip_header).and_then(|v| v.to_str().ok())
            else {
                continue;
            };
            for remote_ip in remote_ips.split(',').map(str::trim) {
                if let Ok(remote_ip_addr) = IpAddr::from_str(remote_ip) {
                    if self
                        .config
                        .trusted_cidrs
                        .iter()
                        .any(|cidr| cidr.contains(&remote_ip_addr))
                    {
                        return ClientIp(Some(remote_ip_addr));
                    }
                }
            }
        }

        ClientIp(remote_ip)
    }
}

impl<S, B> Service<ServerContext, Request<B>> for ClientIpService<S>
where
    S: Service<ServerContext, Request<B>> + Send + Sync + 'static,
    B: Send,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        let client_ip = self.get_client_ip(cx, req.headers());
        cx.extensions_mut().insert(client_ip);

        self.service.call(cx, req).await
    }
}

#[cfg(test)]
mod client_ip_tests {
    use std::{net::SocketAddr, str::FromStr};

    use http::{HeaderValue, Method};
    use motore::{Service, layer::Layer};
    use volo::net::Address;

    use crate::{
        body::BodyConversion,
        context::ServerContext,
        server::{
            route::{Route, get},
            utils::client_ip::{ClientIp, ClientIpConfig, ClientIpLayer},
        },
        utils::test_helpers::simple_req,
    };

    #[tokio::test]
    async fn test_client_ip() {
        async fn handler(ClientIp(client_ip): ClientIp) -> String {
            client_ip.unwrap().to_string()
        }

        let route: Route<&str> = Route::new(get(handler));
        let service = ClientIpLayer::new()
            .with_config(
                ClientIpConfig::default().with_trusted_cidrs(vec!["10.0.0.0/8".parse().unwrap()]),
            )
            .layer(route);

        let mut cx = ServerContext::new(Address::from(
            SocketAddr::from_str("10.0.0.1:8080").unwrap(),
        ));

        // Test case 1: no remote ip header
        let req = simple_req(Method::GET, "/", "");
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!("10.0.0.1", resp.into_string().await.unwrap());

        // Test case 2: with remote ip header
        let mut req = simple_req(Method::GET, "/", "");
        req.headers_mut()
            .insert("X-Real-IP", HeaderValue::from_static("10.0.0.2"));
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!("10.0.0.2", resp.into_string().await.unwrap());

        let mut req = simple_req(Method::GET, "/", "");
        req.headers_mut()
            .insert("X-Forwarded-For", HeaderValue::from_static("10.0.1.0"));
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!("10.0.1.0", resp.into_string().await.unwrap());

        // Test case 3: with untrusted remote ip
        let mut req = simple_req(Method::GET, "/", "");
        req.headers_mut()
            .insert("X-Real-IP", HeaderValue::from_static("11.0.0.1"));
        let resp = service.call(&mut cx, req).await.unwrap();
        assert_eq!("10.0.0.1", resp.into_string().await.unwrap());
    }
}
