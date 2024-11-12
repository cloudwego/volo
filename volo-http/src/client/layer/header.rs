//! [`Layer`]s for inserting header to requests.
//!
//! - [`Header`] inserts any [`HeaderName`] and [`HeaderValue`]
//! - [`Host`] inserts the given `Host` or a `Host` generated by the target hostname or target
//!   address with its scheme and port.
//! - [`UserAgent`] inserts the given `User-Agent` or a `User-Agent` generated by the current
//!   package information.

use std::{error::Error, future::Future, ops::Deref};

use http::{
    header::{self, HeaderName, HeaderValue},
    uri::Scheme,
};
use motore::{layer::Layer, service::Service};
use volo::{
    context::{Context, Endpoint},
    net::Address,
};

use crate::{
    client::{dns::Port, target::is_default_port},
    error::client::{builder_error, Result},
    request::ClientRequest,
};

/// [`Layer`] for inserting a header to requests.
#[derive(Clone, Debug)]
pub struct Header {
    key: HeaderName,
    val: HeaderValue,
}

impl Header {
    /// Create a new [`Header`] layer for inserting a header to requests.
    ///
    /// This function takes [`HeaderName`] and [`HeaderValue`], users should create it by
    /// themselves.
    ///
    /// For using string types directly, see [`Header::try_new`].
    pub fn new(key: HeaderName, val: HeaderValue) -> Self {
        Self { key, val }
    }

    /// Create a new [`Header`] layer for inserting a header to requests.
    ///
    /// This function takes any types that can be converted into [`HeaderName`] or [`HeaderValue`].
    /// If the values are invalid [`HeaderName`] or [`HeaderValue`], an [`ClientError`] with
    /// [`ErrorKind::Builder`] will be returned.
    ///
    /// [`ClientError`]: crate::error::client::ClientError
    /// [`ErrorKind::Builder`]: crate::error::client::ErrorKind::Builder
    pub fn try_new<K, V>(key: K, val: V) -> Result<Self>
    where
        K: TryInto<HeaderName>,
        K::Error: Error + Send + Sync + 'static,
        V: TryInto<HeaderValue>,
        V::Error: Error + Send + Sync + 'static,
    {
        let key = key.try_into().map_err(builder_error)?;
        let val = val.try_into().map_err(builder_error)?;

        Ok(Self::new(key, val))
    }
}

impl<S> Layer<S> for Header {
    type Service = HeaderService<S>;

    fn layer(self, inner: S) -> Self::Service {
        HeaderService {
            inner,
            key: self.key,
            val: self.val,
        }
    }
}

/// [`Service`] generated by [`Header`].
///
/// See [`Header`], [`Header::new`] and [`Header::try_new`] for more details.
pub struct HeaderService<S> {
    inner: S,
    key: HeaderName,
    val: HeaderValue,
}

impl<Cx, B, S> Service<Cx, ClientRequest<B>> for HeaderService<S>
where
    S: Service<Cx, ClientRequest<B>>,
{
    type Response = S::Response;
    type Error = S::Error;

    fn call(
        &self,
        cx: &mut Cx,
        mut req: ClientRequest<B>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        req.headers_mut().insert(self.key.clone(), self.val.clone());
        self.inner.call(cx, req)
    }
}

/// [`Layer`] for inserting `Host` into the request header.
///
/// See [`Host::new`] and [`Host::auto`] for more details.
pub struct Host {
    val: Option<HeaderValue>,
}

impl Host {
    /// Create a new [`Host`] layer that inserts `Host` into the request header.
    ///
    /// Note that the layer only inserts it if there is no `Host`
    pub fn new(val: HeaderValue) -> Self {
        Self { val: Some(val) }
    }

    /// Create a new [`Host`] layer that inserts `Host` by the current target host name, port or
    /// address.
    ///
    /// Note that the layer only inserts it if there is no `Host`.
    ///
    /// This layer also does nothing if there is no target hostname and the target address is not
    /// an <ip:port> address (such as a unix domain socket).
    pub fn auto() -> Self {
        Self { val: None }
    }
}

impl<S> Layer<S> for Host {
    type Service = HostService<S>;

    fn layer(self, inner: S) -> Self::Service {
        HostService {
            inner,
            val: self.val,
        }
    }
}

/// [`Service`] generated by [`Host`].
///
/// See [`Host`] and [`Host::new`] for more details.
pub struct HostService<S> {
    inner: S,
    val: Option<HeaderValue>,
}

// keep it as a separate function to facilitate unit testing
fn gen_host(
    scheme: &Scheme,
    name: &str,
    addr: Option<&Address>,
    port: Option<u16>,
) -> Option<HeaderValue> {
    if name.is_empty() {
        match addr? {
            Address::Ip(sa) => {
                if is_default_port(scheme, sa.port()) {
                    HeaderValue::try_from(format!("{}", sa.ip())).ok()
                } else {
                    HeaderValue::try_from(format!("{}", sa)).ok()
                }
            }
            #[cfg(target_family = "unix")]
            Address::Unix(_) => None,
        }
    } else {
        if let Some(port) = port {
            if !is_default_port(scheme, port) {
                return HeaderValue::try_from(format!("{name}:{port}")).ok();
            }
        }
        HeaderValue::from_str(name).ok()
    }
}

fn gen_host_by_ep(ep: &Endpoint) -> Option<HeaderValue> {
    let scheme = ep.get::<Scheme>()?;
    let name = ep.service_name_ref();
    let addr = ep.address.as_ref();
    let port = ep.get::<Port>().map(Deref::deref).cloned();
    gen_host(scheme, name, addr, port)
}

impl<Cx, B, S> Service<Cx, ClientRequest<B>> for HostService<S>
where
    Cx: Context,
    S: Service<Cx, ClientRequest<B>>,
{
    type Response = S::Response;
    type Error = S::Error;

    fn call(
        &self,
        cx: &mut Cx,
        mut req: ClientRequest<B>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        if !req.headers().contains_key(header::HOST) {
            if let Some(val) = gen_host_by_ep(cx.rpc_info().callee()) {
                req.headers_mut().insert(header::HOST, val);
            } else if let Some(val) = &self.val {
                req.headers_mut().insert(header::HOST, val.clone());
            }
        }
        self.inner.call(cx, req)
    }
}

const PKG_NAME_WITH_VER: &str = concat!(env!("CARGO_PKG_NAME"), '/', env!("CARGO_PKG_VERSION"));

/// [`Layer`] for inserting `User-Agent` into the request header.
///
/// See [`UserAgent::new`] for more details.
pub struct UserAgent {
    val: HeaderValue,
}

impl UserAgent {
    /// Create a new [`UserAgent`] layer that inserts `User-Agent` into the request header.
    ///
    /// Note that the layer only inserts it if there is no `User-Agent`
    pub fn new(val: HeaderValue) -> Self {
        Self { val }
    }

    /// Create a new [`UserAgent`] layer with the package name and package version as its default
    /// value.
    ///
    /// Note that the layer only inserts it if there is no `User-Agent`
    pub fn auto() -> Self {
        Self {
            val: HeaderValue::from_static(PKG_NAME_WITH_VER),
        }
    }
}

impl<S> Layer<S> for UserAgent {
    type Service = UserAgentService<S>;

    fn layer(self, inner: S) -> Self::Service {
        UserAgentService {
            inner,
            val: self.val,
        }
    }
}

/// [`Service`] generated by [`UserAgent`].
///
/// See [`UserAgent`] and [`UserAgent::new`] for more details.
pub struct UserAgentService<S> {
    inner: S,
    val: HeaderValue,
}

impl<Cx, B, S> Service<Cx, ClientRequest<B>> for UserAgentService<S>
where
    S: Service<Cx, ClientRequest<B>>,
{
    type Response = S::Response;
    type Error = S::Error;

    fn call(
        &self,
        cx: &mut Cx,
        mut req: ClientRequest<B>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        if !req.headers().contains_key(header::USER_AGENT) {
            req.headers_mut()
                .insert(header::USER_AGENT, self.val.clone());
        }
        self.inner.call(cx, req)
    }
}

#[cfg(test)]
mod layer_header_tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    use http::uri::Scheme;
    use volo::net::Address;

    use crate::client::layer::header::gen_host;

    fn gen_ipv4addr(port: u16) -> Address {
        // 127.0.0.1:port
        Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port,
        ))
    }

    fn gen_ipv6addr(port: u16) -> Address {
        // [::1]:port
        Address::Ip(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)),
            port,
        ))
    }

    #[test]
    fn gen_host_test() {
        // no host, no addr
        assert_eq!(gen_host(&Scheme::HTTP, "", None, Some(80)), None);

        // host without port
        assert_eq!(
            gen_host(&Scheme::HTTP, "github.com", None, None).unwrap(),
            "github.com"
        );
        // host with default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "github.com", None, Some(80)).unwrap(),
            "github.com"
        );
        // host with non-default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "github.com", None, Some(8000)).unwrap(),
            "github.com:8000"
        );
        assert_eq!(
            gen_host(&Scheme::HTTP, "github.com", None, Some(443)).unwrap(),
            "github.com:443"
        );

        // same test case as above, but with a resolved address
        // host without port
        assert_eq!(
            gen_host(&Scheme::HTTP, "github.com", Some(&gen_ipv4addr(80)), None).unwrap(),
            "github.com"
        );
        // host with default port
        assert_eq!(
            gen_host(
                &Scheme::HTTP,
                "github.com",
                Some(&gen_ipv4addr(80)),
                Some(80)
            )
            .unwrap(),
            "github.com"
        );
        // host with non-default port
        assert_eq!(
            gen_host(
                &Scheme::HTTP,
                "github.com",
                Some(&gen_ipv4addr(8000)),
                Some(8000)
            )
            .unwrap(),
            "github.com:8000"
        );
        assert_eq!(
            gen_host(
                &Scheme::HTTP,
                "github.com",
                Some(&gen_ipv4addr(8000)),
                Some(443)
            )
            .unwrap(),
            "github.com:443"
        );

        // ipv4 addr with default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv4addr(80)), None).unwrap(),
            "127.0.0.1"
        );
        // ipv4 addr with non-default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv4addr(8000)), None).unwrap(),
            "127.0.0.1:8000"
        );
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv4addr(443)), None).unwrap(),
            "127.0.0.1:443"
        );

        // althrough these cases are impossible to happen, we also test it
        // ipv4 addr with default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv4addr(80)), Some(8888)).unwrap(),
            "127.0.0.1"
        );
        // ipv4 addr with non-default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv4addr(8000)), Some(8888)).unwrap(),
            "127.0.0.1:8000"
        );
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv4addr(443)), Some(8888)).unwrap(),
            "127.0.0.1:443"
        );

        // ipv6 addr with default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv6addr(80)), None).unwrap(),
            "::1"
        );
        // ipv6 addr with non-default port
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv6addr(8000)), None).unwrap(),
            "[::1]:8000"
        );
        assert_eq!(
            gen_host(&Scheme::HTTP, "", Some(&gen_ipv6addr(443)), None).unwrap(),
            "[::1]:443"
        );
    }

    #[cfg(feature = "__tls")]
    #[test]
    fn gen_host_with_tls_test() {
        // no host, no addr
        assert_eq!(gen_host(&Scheme::HTTPS, "", None, Some(443)), None);

        // host without port
        assert_eq!(
            gen_host(&Scheme::HTTPS, "github.com", None, None).unwrap(),
            "github.com"
        );
        // host with default port
        assert_eq!(
            gen_host(&Scheme::HTTPS, "github.com", None, Some(443)).unwrap(),
            "github.com"
        );
        // host with non-default port
        assert_eq!(
            gen_host(&Scheme::HTTPS, "github.com", None, Some(4430)).unwrap(),
            "github.com:4430"
        );
        assert_eq!(
            gen_host(&Scheme::HTTPS, "github.com", None, Some(80)).unwrap(),
            "github.com:80"
        );

        // ipv4 addr with default port
        assert_eq!(
            gen_host(&Scheme::HTTPS, "", Some(&gen_ipv4addr(443)), None).unwrap(),
            "127.0.0.1"
        );
        // ipv4 addr with non-default port
        assert_eq!(
            gen_host(&Scheme::HTTPS, "", Some(&gen_ipv4addr(4430)), None).unwrap(),
            "127.0.0.1:4430"
        );
        assert_eq!(
            gen_host(&Scheme::HTTPS, "", Some(&gen_ipv4addr(80)), None).unwrap(),
            "127.0.0.1:80"
        );

        // ipv6 addr with default port
        assert_eq!(
            gen_host(&Scheme::HTTPS, "", Some(&gen_ipv6addr(443)), None).unwrap(),
            "::1"
        );
        // ipv6 addr with non-default port
        assert_eq!(
            gen_host(&Scheme::HTTPS, "", Some(&gen_ipv6addr(4430)), None).unwrap(),
            "[::1]:4430"
        );
        assert_eq!(
            gen_host(&Scheme::HTTPS, "", Some(&gen_ipv6addr(80)), None).unwrap(),
            "[::1]:80"
        );
    }
}
