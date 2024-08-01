//! HTTP target address related types
//!
//! See [`Target`], [`RemoteTarget`] for more details.

use std::net::{IpAddr, SocketAddr};

use faststr::FastStr;
use http::{uri::Scheme, HeaderValue, Uri};
use volo::{context::Endpoint, net::Address};

use crate::{
    client::callopt::CallOpt,
    error::{client::bad_scheme, ClientError},
    utils::consts,
};

/// Function for parsing [`Target`] and [`CallOpt`] to update [`Endpoint`].
///
/// The `TargetParser` usually used for service discover. It can update [`Endpoint` ]from
/// [`Target`] and [`CallOpt`], and the service discover will resolve the [`Endpoint`] to
/// [`Address`]\(es\) and access them.
pub type TargetParser = fn(Target, Option<&CallOpt>, &mut Endpoint);

/// HTTP target server descriptor
#[derive(Clone, Debug, Default)]
pub enum Target {
    #[default]
    /// No target specified
    None,
    /// Remote target, supports service name (domain name by default) or ip address
    Remote(RemoteTarget),
    /// Local target, usually using a unix domain socket.
    #[cfg(target_family = "unix")]
    Local(std::os::unix::net::SocketAddr),
}

/// Remote part of [`Target`]
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    /// The target address
    pub addr: RemoteTargetAddress,
    /// Target port, its default value depends on scheme
    pub port: Option<u16>,
    /// Use https for transporting
    #[cfg(feature = "__tls")]
    pub https: bool,
}

/// Remote address of [`RemoteTarget`]
#[derive(Clone, Debug)]
pub enum RemoteTargetAddress {
    /// Ip address
    Ip(IpAddr),
    /// Service name, usually a domain name
    Name(FastStr),
}

impl Target {
    /// Build a `Target` from `Uri`.
    ///
    /// If there is no host, `None` will be returned. If there is a host, but the uri has something
    /// invalid (e.g., unsupported scheme), an error will be returned.
    pub fn from_uri(uri: &Uri) -> Option<Result<Self, ClientError>> {
        let host = uri.host()?;
        let Some(https) = is_https(uri) else {
            tracing::error!("[Volo-HTTP] unsupported scheme: {:?}.", uri.scheme());
            return Some(Err(bad_scheme()));
        };
        #[cfg(not(feature = "__tls"))]
        if https {
            tracing::error!("[Volo-HTTP] https is not allowed when feature `tls` is not enabled.");
            return Some(Err(bad_scheme()));
        }

        let addr = match host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
        {
            Ok(ip) => RemoteTargetAddress::Ip(ip),
            Err(_) => RemoteTargetAddress::Name(FastStr::from_string(host.to_owned())),
        };
        let port = uri.port_u16();
        Some(Ok(Self::Remote(RemoteTarget {
            addr,
            port,
            #[cfg(feature = "__tls")]
            https,
        })))
    }

    /// Build a `Target` from an address.
    pub fn from_address<A>(addr: A) -> Self
    where
        A: Into<Address>,
    {
        Self::from(addr.into())
    }

    /// Build a `Target` from a host name.
    ///
    /// Note that the `host` must be a host name, it will be used for service discover.
    ///
    /// It should NOT be an address or something with port.
    ///
    /// If you have a uri and you are not sure if the host is a host, try `from_uri`.
    pub fn from_host<S>(host: S) -> Self
    where
        S: AsRef<str>,
    {
        Self::Remote(RemoteTarget {
            addr: RemoteTargetAddress::Name(FastStr::from_string(host.as_ref().to_owned())),
            port: None,
            #[cfg(feature = "__tls")]
            https: false,
        })
    }

    /// Return if the `Target` is `None`.
    pub fn is_none(&self) -> bool {
        matches!(self, Target::None)
    }

    /// Get a reference of the [`RemoteTarget`].
    pub fn remote_ref(&self) -> Option<&RemoteTarget> {
        match self {
            Self::Remote(remote) => Some(remote),
            _ => None,
        }
    }

    /// Get a mutable reference of the [`RemoteTarget`].
    pub fn remote_mut(&mut self) -> Option<&mut RemoteTarget> {
        match self {
            Self::Remote(remote) => Some(remote),
            _ => None,
        }
    }

    /// Set remote port and return a new target.
    pub fn set_port(&mut self, port: u16) {
        if let Some(rt) = self.remote_mut() {
            rt.port = Some(port);
        }
    }

    /// Set if use https for the target.
    ///
    /// If the [`Target`] cannot use https ([`Target::None`] or [`Target::Local`]), this function
    /// will do nothing.
    #[cfg(feature = "__tls")]
    pub fn set_https(&mut self, https: bool) {
        if let Some(rt) = self.remote_mut() {
            rt.set_https(https);
        }
    }

    /// Check if the target uses https.
    ///
    /// If the [`Target`] cannot use https ([`Target::None`] or [`Target::Local`]), this function
    /// will return `false`.
    pub fn is_https(&self) -> bool {
        #[cfg(feature = "__tls")]
        if let Some(rt) = self.remote_ref() {
            rt.is_https()
        } else {
            false
        }
        #[cfg(not(feature = "__tls"))]
        false
    }

    /// Return the remote [`IpAddr`] if the [`Target`] is an IP address.
    pub fn remote_ip(&self) -> Option<&IpAddr> {
        if let Self::Remote(rt) = &self {
            if let RemoteTargetAddress::Ip(ip) = &rt.addr {
                return Some(ip);
            }
        }
        None
    }

    /// Return the remote host name if the [`Target`] is a host name.
    pub fn remote_host(&self) -> Option<&FastStr> {
        if let Self::Remote(rt) = &self {
            if let RemoteTargetAddress::Name(name) = &rt.addr {
                return Some(name);
            }
        }
        None
    }

    /// Return the unix socket address if the [`Target`] is it.
    #[cfg(target_family = "unix")]
    pub fn unix_socket_addr(&self) -> Option<&std::os::unix::net::SocketAddr> {
        if let Self::Local(sa) = &self {
            Some(sa)
        } else {
            None
        }
    }

    /// Return the port if the [`Target`] is a remote address and the port is given.
    pub fn port(&self) -> Option<u16> {
        if let Some(remote) = self.remote_ref() {
            remote.port
        } else {
            None
        }
    }

    /// Generate a `HeaderValue` for `Host` in HTTP headers.
    pub fn gen_host(&self) -> Option<HeaderValue> {
        HeaderValue::try_from(self.try_to_string()?).ok()
    }

    fn is_default_port(&self) -> bool {
        let Some(rt) = self.remote_ref() else {
            // Local address does not have port.
            return false;
        };
        let Some(port) = rt.port else {
            // `None` means using default port.
            return true;
        };
        #[cfg(feature = "__tls")]
        if rt.https {
            return port == consts::HTTPS_DEFAULT_PORT;
        }
        port == consts::HTTP_DEFAULT_PORT
    }

    fn try_to_string(&self) -> Option<String> {
        let rt = self.remote_ref()?;
        let without_port = self.is_default_port();
        match rt.addr {
            RemoteTargetAddress::Ip(ref ip) => {
                if without_port {
                    return Some(ip.to_string());
                }
                // SAFETY: the port must exist if the port is non-default one
                let port = rt.port.unwrap();
                if ip.is_ipv6() {
                    Some(format!("[{ip}]:{port}"))
                } else {
                    Some(format!("{ip}:{port}"))
                }
            }
            RemoteTargetAddress::Name(ref name) => {
                if without_port {
                    return Some(name.to_string());
                }
                // SAFETY: the port must exist if the port is non-default one
                let port = rt.port.unwrap();
                Some(format!("{name}:{port}"))
            }
        }
    }
}

impl From<Address> for Target {
    fn from(value: Address) -> Self {
        match value {
            Address::Ip(sa) => Target::Remote(RemoteTarget {
                addr: RemoteTargetAddress::Ip(sa.ip()),
                port: Some(sa.port()),
                #[cfg(feature = "__tls")]
                https: false,
            }),
            #[cfg(target_family = "unix")]
            Address::Unix(uds) => Target::Local(uds),
        }
    }
}

impl TryFrom<Target> for Address {
    type Error = Target;

    fn try_from(value: Target) -> Result<Self, Self::Error> {
        match value {
            Target::None => Err(value),
            #[cfg(target_family = "unix")]
            Target::Local(sa) => Ok(Address::Unix(sa)),
            Target::Remote(rt) => {
                let port = rt.port();
                if let RemoteTargetAddress::Ip(ip) = rt.addr {
                    Ok(Address::Ip(SocketAddr::new(ip, port)))
                } else {
                    Err(Target::Remote(rt))
                }
            }
        }
    }
}

impl RemoteTarget {
    /// Get the target port for the [`RemoteTarget`].
    ///
    /// If the port has not been set, it will return a default port based on if https is enabled.
    pub fn port(&self) -> u16 {
        if let Some(port) = self.port {
            return port;
        }
        #[cfg(feature = "__tls")]
        if self.https {
            return consts::HTTPS_DEFAULT_PORT;
        }
        consts::HTTP_DEFAULT_PORT
    }

    /// Set if use https for the target.
    #[cfg(feature = "__tls")]
    pub fn set_https(&mut self, https: bool) {
        self.https = https;
    }

    /// Check if the target uses https.
    #[cfg(feature = "__tls")]
    pub fn is_https(&self) -> bool {
        self.https
    }
}

fn is_https(uri: &Uri) -> Option<bool> {
    let Some(scheme) = uri.scheme() else {
        return Some(false);
    };
    if scheme == &Scheme::HTTPS {
        return Some(true);
    }
    if scheme == &Scheme::HTTP {
        return Some(false);
    }
    None
}

#[cfg(test)]
mod target_tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    use http::uri::Uri;
    use volo::net::Address;

    use super::Target;

    #[test]
    fn test_from_uri() {
        // no domain name
        let target = Target::from_uri(&Uri::from_static("/api/v1/config"));
        assert!(target.is_none());

        // invalid scheme
        let target = Target::from_uri(&Uri::from_static("ftp://github.com"));
        assert!(matches!(target, Some(Err(_))));

        // ipv4 only
        let target = Target::from_uri(&Uri::from_static("10.0.0.1"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(
            target.remote_ip().unwrap().to_string(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)).to_string(),
        );
        assert_eq!(target.port(), None);
        assert!(!target.is_https());

        // ipv4 with port
        let target = Target::from_uri(&Uri::from_static("10.0.0.1:8000"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(
            target.remote_ip().unwrap().to_string(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)).to_string(),
        );
        assert_eq!(target.port(), Some(8000));
        assert!(!target.is_https());

        // ipv6 with port
        let target = Target::from_uri(&Uri::from_static("[ff::1]:8000"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(
            target.remote_ip().unwrap().to_string(),
            IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 1)).to_string(),
        );
        assert_eq!(target.port(), Some(8000));
        assert!(!target.is_https());

        // domain name only
        let target = Target::from_uri(&Uri::from_static("github.com"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), None);
        assert!(!target.is_https());

        // domain with scheme (http)
        let target = Target::from_uri(&Uri::from_static("http://github.com/"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), None);
        assert!(!target.is_https());

        // domain with port
        let target = Target::from_uri(&Uri::from_static("github.com:8000"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), Some(8000));
        assert!(!target.is_https());

        // domain with scheme (http) and port
        let target = Target::from_uri(&Uri::from_static("http://github.com:8000/"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), Some(8000));
        assert!(!target.is_https());
    }

    #[cfg(not(feature = "__tls"))]
    #[test]
    fn test_from_uri_without_tls() {
        // domain with scheme (https)

        use crate::error::client::bad_scheme;
        let target = Target::from_uri(&Uri::from_static("https://github.com"));
        assert!(matches!(target, Some(Err(_))));
        assert_eq!(
            format!("{}", target.unwrap().unwrap_err()),
            format!("{}", bad_scheme()),
        );

        // domain with scheme (https) and port
        let target = Target::from_uri(&Uri::from_static("https://github.com:8000/"));
        assert!(matches!(target, Some(Err(_))));
        assert_eq!(
            format!("{}", target.unwrap().unwrap_err()),
            format!("{}", bad_scheme()),
        );
    }

    #[cfg(feature = "__tls")]
    #[test]
    fn test_from_uri_with_tls() {
        // domain with scheme (https)
        let target = Target::from_uri(&Uri::from_static("https://github.com"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), None);
        assert!(target.is_https());

        // domain with scheme (https) and port
        let target = Target::from_uri(&Uri::from_static("https://github.com:8000/"));
        assert!(matches!(target, Some(Ok(_))));
        let target = target.unwrap().unwrap();
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), Some(8000));
        assert!(target.is_https());
    }

    #[test]
    fn test_from_ip_address() {
        // IPv4
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let port = 8000;
        let addr = Address::Ip(SocketAddr::new(ip, port));
        let target = Target::from_address(addr);
        assert_eq!(target.remote_ip(), Some(&ip));
        assert_eq!(target.port(), Some(port));
        assert!(!target.is_https());

        // IPv6
        let ip = IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 0));
        let port = 8000;
        let addr = Address::Ip(SocketAddr::new(ip, port));
        let target = Target::from_address(addr);
        assert_eq!(target.remote_ip(), Some(&ip));
        assert_eq!(target.port(), Some(port));
        assert!(!target.is_https());
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_from_uds_address() {
        #[derive(Debug, PartialEq, Eq)]
        struct SocketAddr {
            addr: libc::sockaddr_un,
            len: libc::socklen_t,
        }

        let uds = std::os::unix::net::SocketAddr::from_pathname("/tmp/test.sock").unwrap();
        let addr = Address::Unix(uds.clone());
        let target = Target::from_address(addr);

        // Use a same struct with `PartialEq` and `Eq` and transmute them for comparing.
        let uds: SocketAddr = unsafe { std::mem::transmute(uds) };
        let target_uds: SocketAddr =
            unsafe { std::mem::transmute(target.unix_socket_addr().unwrap().to_owned()) };
        assert_eq!(target_uds, uds);
        assert!(target.port().is_none());
        assert!(!target.is_https());
    }

    #[test]
    fn test_from_host() {
        let target = Target::from_host("github.com");
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert!(target.port().is_none());
        assert!(!target.is_https());
    }

    #[test]
    fn test_uri_with_port() {
        // domain name only
        let target = Target::from_uri(&Uri::from_static("github.com"));
        assert!(matches!(target, Some(Ok(_))));
        let mut target = target.unwrap().unwrap();
        target.set_port(8000);
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), Some(8000));

        // domain name with port and override it
        let target = Target::from_uri(&Uri::from_static("github.com:80"));
        assert!(matches!(target, Some(Ok(_))));
        let mut target = target.unwrap().unwrap();
        target.set_port(8000);
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), Some(8000));
    }

    #[test]
    fn test_ip_with_port() {
        // IPv4
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let port = 8000;
        let addr = Address::Ip(SocketAddr::new(ip, port));
        let mut target = Target::from_address(addr);
        target.set_port(80);
        assert_eq!(target.remote_ip(), Some(&ip));
        assert_eq!(target.port(), Some(80));

        // IPv6
        let ip = IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 1));
        let port = 8000;
        let addr = Address::Ip(SocketAddr::new(ip, port));
        let mut target = Target::from_address(addr);
        target.set_port(80);
        assert_eq!(target.remote_ip(), Some(&ip));
        assert_eq!(target.port(), Some(80));
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn test_uds_with_port() {
        let uds = std::os::unix::net::SocketAddr::from_pathname("/tmp/test.sock").unwrap();
        let addr = Address::Unix(uds.clone());
        let mut target = Target::from_address(addr);
        assert!(target.port().is_none());
        target.set_port(80);
        // uds does not have port
        assert!(target.port().is_none());
    }

    #[test]
    fn test_host_with_port() {
        let mut target = Target::from_host("github.com");
        let port = 8000;
        target.set_port(port);
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert_eq!(target.port(), Some(port));
    }

    #[cfg(feature = "__tls")]
    #[test]
    fn test_uri_with_https() {
        // domain name only
        let target = Target::from_uri(&Uri::from_static("github.com"));
        assert!(matches!(target, Some(Ok(_))));
        let mut target = target.unwrap().unwrap();
        target.set_https(true);
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert!(target.is_https());

        // domain name with http and override it
        let target = Target::from_uri(&Uri::from_static("http://github.com"));
        assert!(matches!(target, Some(Ok(_))));
        let mut target = target.unwrap().unwrap();
        target.set_https(true);
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert!(target.is_https());
    }

    #[cfg(feature = "__tls")]
    #[test]
    fn test_ip_with_https() {
        // IPv4
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let port = 8000;
        let addr = Address::Ip(SocketAddr::new(ip, port));
        let mut target = Target::from_address(addr);
        target.set_https(true);
        assert_eq!(target.remote_ip(), Some(&ip));
        assert!(target.is_https());

        // IPv6
        let ip = IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 0));
        let port = 8000;
        let addr = Address::Ip(SocketAddr::new(ip, port));
        let mut target = Target::from_address(addr);
        target.set_https(true);
        assert_eq!(target.remote_ip(), Some(&ip));
        assert!(target.is_https());
    }

    #[cfg(all(feature = "__tls", target_family = "unix"))]
    #[test]
    fn test_uds_with_https() {
        let uds = std::os::unix::net::SocketAddr::from_pathname("/tmp/test.sock").unwrap();
        let addr = Address::Unix(uds.clone());
        let mut target = Target::from_address(addr);
        assert!(target.port().is_none());
        target.set_https(true);
        // uds does not have port
        assert!(!target.is_https());
    }

    #[cfg(feature = "__tls")]
    #[test]
    fn test_host_with_https() {
        let mut target = Target::from_host("github.com");
        target.set_https(true);
        assert_eq!(target.remote_host().unwrap(), "github.com");
        assert!(target.is_https());
    }

    #[cfg(feature = "__tls")]
    #[test]
    fn test_gen_host() {
        fn gen_host_to_string(target: &Target) -> Option<String> {
            let host = target.gen_host()?;
            Some(host.to_str().map(ToOwned::to_owned).unwrap_or_default())
        }

        // ipv4 with default http port
        let target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            80,
        )));
        assert_eq!(gen_host_to_string(&target).as_deref(), Some("127.0.0.1"));
        // ipv4 with non-default http port
        let target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            443,
        )));
        assert_eq!(
            gen_host_to_string(&target).as_deref(),
            Some("127.0.0.1:443")
        );
        // ipv4 with default https port
        let mut target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            443,
        )));
        target.set_https(true);
        assert_eq!(gen_host_to_string(&target).as_deref(), Some("127.0.0.1"));
        // ipv4 with non-default https port
        let mut target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            80,
        )));
        target.set_https(true);
        assert_eq!(gen_host_to_string(&target).as_deref(), Some("127.0.0.1:80"));

        // ipv6 with default http port
        let target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 1)),
            80,
        )));
        assert_eq!(gen_host_to_string(&target).as_deref(), Some("ff::1"));
        // ipv6 with non-default http port
        let target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 1)),
            443,
        )));
        assert_eq!(gen_host_to_string(&target).as_deref(), Some("[ff::1]:443"));
        // ipv6 with default https port
        let mut target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 1)),
            443,
        )));
        target.set_https(true);
        assert_eq!(gen_host_to_string(&target).as_deref(), Some("ff::1"));
        // ipv6 with non-default https port
        let mut target = Target::from_address(Address::Ip(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0xff, 0, 0, 0, 0, 0, 0, 1)),
            80,
        )));
        target.set_https(true);
        assert_eq!(gen_host_to_string(&target).as_deref(), Some("[ff::1]:80"));
    }
}
