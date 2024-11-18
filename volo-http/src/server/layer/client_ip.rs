use std::{borrow::Cow, net::IpAddr, str::FromStr};

use http::{HeaderMap, HeaderName};
use ipnet::IpNet;
use motore::{layer::Layer, Service};
use volo::{context::Context, net::Address};

use crate::{
    context::ServerContext, server::IntoResponse,
    utils::macros::impl_deref_and_deref_mut,
};
use crate::request::Request;
use crate::response::Response;

/// [`Layer`] for extracting client ip
///
/// See [`ClientIP`] for more details.
#[derive(Clone)]
pub struct ClientIPLayerImpl<H> {
    config: ClientIPConfig,
    handler: H,
}

/// [`Layer`] for extracting client ip
///
/// See [`ClientIP`] for more details.
pub type ClientIPLayer =
    ClientIPLayerImpl<fn(&ClientIPConfig, &ServerContext, &HeaderMap) -> ClientIP>;

impl Default for ClientIPLayer {
    fn default() -> Self {
        Self::new(default_client_ip_handler)
    }
}

impl<H> ClientIPLayerImpl<H> {
    /// Create a new [`ClientIPLayerImpl`]
    pub fn new(handler: H) -> Self {
        Self {
            config: ClientIPConfig::default(),
            handler,
        }
    }

    /// Create a new [`ClientIPLayerImpl`] with the given [`ClientIPConfig`]
    pub fn with_config(self, config: ClientIPConfig) -> Self {
        Self {
            config,
            handler: self.handler,
        }
    }
}

impl<S, H> Layer<S> for ClientIPLayerImpl<H>
where
    S: Send + Sync + 'static,
{
    type Service = ClientIPService<S, H>;

    fn layer(self, inner: S) -> Self::Service {
        ClientIPService {
            service: inner,
            config: self.config,
            handler: self.handler,
        }
    }
}

/// Config for extract client ip
#[derive(Clone, Debug)]
pub struct ClientIPConfig {
    remote_ip_headers: Vec<HeaderName>,
    trusted_cidrs: Vec<IpNet>,
}

impl Default for ClientIPConfig {
    fn default() -> Self {
        Self {
            remote_ip_headers: vec![
                HeaderName::from_static("x-forwarded-for"),
                HeaderName::from_static("x-real-ip"),
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
    /// See [`ClientIP`] for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::server::layer::ClientIPConfig;
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

    /// Get Real Client IP if it is trusted, otherwise it will just return caller ip.
    ///
    /// See [`ClientIP`] for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use volo_http::server::layer::ClientIPConfig;
    ///
    /// let client_ip_config = ClientIPConfig::new()
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

/// Return real `ClientIP` by parsing the headers in `["X-Forwarded-For", "X-Real-IP"]` if ip is
/// trusted, otherwise it will just return caller ip by calling
/// `cx.rpc_info().caller().address().ip()`.
///
/// If you want to specify your own headers, you can use
/// [`with_remote_ip_headers`](ClientIPConfig::with_remote_ip_headers) to set the
/// headers.
///
/// If you want to specify your own trusted cidrs, you can use
/// [`with_trusted_cidrs`](ClientIPConfig::with_trusted_cidrs) to set the cidrs.
///
/// # Example
///
/// ## Default config
///
/// ```rust
/// use volo_http::server::{
///     layer::{ClientIPConfig, ClientIPLayer},
///     route::{get, Router},
///     Server,
/// };
///
/// async fn index() -> &'static str {
///     "Hello, World"
/// }
///
/// let router: Router = Router::new()
///     .route("/", get(index))
///     .layer(ClientIPLayer::default());
/// ```
///
/// ## With custom config
///
/// ```rust
/// use http::HeaderMap;
/// use volo_http::{
///     context::ServerContext,
///     server::{
///         layer::{ClientIP, ClientIPConfig, ClientIPLayer},
///         route::{get, Router},
///         Server,
///     },
/// };
///
/// async fn index() -> &'static str {
///     "Hello, World"
/// }
///
/// fn client_ip_handler(
///     config: &ClientIPConfig,
///     cx: &ServerContext,
///     headers: &HeaderMap,
/// ) -> ClientIP {
///     unimplemented!()
/// }
///
/// let router: Router = Router::new().route("/", get(index)).layer(
///     ClientIPLayer::new(client_ip_handler).with_config(
///         ClientIPConfig::new()
///             .with_remote_ip_headers(vec!["x-real-ip", "x-forwarded-for"])
///             .unwrap()
///             .with_trusted_cidrs(vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()]),
///     ),
/// );
/// ```
pub struct ClientIP(pub Option<IpAddr>);

impl_deref_and_deref_mut!(ClientIP, Option<IpAddr>, 0);

trait ClientIPHandler<'r> {
    fn call(
        self,
        config: &'r ClientIPConfig,
        cx: &'r ServerContext,
        headers: &'r HeaderMap,
    ) -> ClientIP;
}

impl<'r, F> ClientIPHandler<'r> for F
where
    F: FnOnce(&'r ClientIPConfig, &'r ServerContext, &'r HeaderMap) -> ClientIP,
{
    fn call(
        self,
        config: &'r ClientIPConfig,
        cx: &'r ServerContext,
        headers: &'r HeaderMap,
    ) -> ClientIP {
        self(config, cx, headers)
    }
}

fn default_client_ip_handler(
    config: &ClientIPConfig,
    cx: &ServerContext,
    headers: &HeaderMap,
) -> ClientIP {
    let remote_ip_headers = &config.remote_ip_headers;
    let trusted_cidrs = &config.trusted_cidrs;

    let remote_ip = match cx.rpc_info().caller().address() {
        Some(Address::Ip(socket_addr)) => Some(socket_addr.ip()),
        Some(Address::Unix(_)) => None,
        None => return ClientIP(None),
    };

    if let Some(remote_ip) = remote_ip {
        if !trusted_cidrs
            .iter()
            .any(|cidr| cidr.contains(&IpNet::from(remote_ip)))
        {
            return ClientIP(None);
        }
    }

    for remote_ip_header in remote_ip_headers.iter() {
        let remote_ips = match headers
            .get(remote_ip_header)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').map(|s| s.trim()).collect::<Vec<_>>())
        {
            Some(remote_ips) => remote_ips,
            None => continue,
        };
        for remote_ip in remote_ips.iter() {
            if let Ok(remote_cidr) = IpAddr::from_str(remote_ip) {
                if trusted_cidrs.iter().any(|cidr| cidr.contains(&remote_cidr)) {
                    return ClientIP(Some(remote_cidr));
                }
            }
        }
    }

    if let Some(remote_ip) = remote_ip {
        return ClientIP(Some(remote_ip));
    }

    ClientIP(None)
}

#[derive(Clone)]
pub struct ClientIPService<S, H> {
    service: S,
    config: ClientIPConfig,
    handler: H,
}

impl<S, B, H> Service<ServerContext, Request<B>> for ClientIPService<S, H>
where
    S: Service<ServerContext, Request<B>> + Send + Sync + 'static,
    S::Response: IntoResponse,
    S::Error: IntoResponse,
    B: Send,
    H: for<'r> ClientIPHandler<'r> + Clone + Sync,
{
    type Response = Response;
    type Error = S::Error;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        let client_ip = self.handler.clone().call(&self.config, cx, req.headers());
        cx.rpc_info_mut().caller_mut().tags.insert(client_ip);

        Ok(self.service.call(cx, req).await.into_response())
    }
}

#[cfg(test)]
mod client_ip_tests {
    use std::{net::SocketAddr, str::FromStr};

    use http::{HeaderValue, Method};
    use motore::{layer::Layer, Service};
    use volo::net::Address;

    use crate::{
        body::BodyConversion,
        context::ServerContext,
        server::{
            layer::{client_ip::ClientIPLayer, ClientIP, ClientIPConfig},
            route::{get, Route},
        },
        utils::test_helpers::simple_req,
    };

    #[tokio::test]
    async fn test_client_ip() {
        async fn handler(client_ip: ClientIP) -> String {
            client_ip.unwrap().to_string()
        }

        let route: Route<&str> = Route::new(get(handler));
        let service = ClientIPLayer::default()
            .with_config(
                ClientIPConfig::default().with_trusted_cidrs(vec!["10.0.0.0/8".parse().unwrap()]),
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
