//! HTTP target address related types
//!
//! See [`Target`], [`RemoteTarget`] for more details.

use std::{
    borrow::Cow,
    fmt,
    net::{IpAddr, SocketAddr},
};

use faststr::FastStr;
use http::uri::{Scheme, Uri};
use volo::{client::Apply, context::Context, net::Address};

use super::utils::{get_default_port, is_default_port};
use crate::{
    client::dns::Port,
    context::ClientContext,
    error::{
        ClientError,
        client::{Result, bad_scheme, no_address, port_unavailable, scheme_unavailable},
    },
    utils::consts,
};

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

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::None => f.write_str("none"),
            Target::Remote(rt) => write!(f, "{rt}"),
            #[cfg(target_family = "unix")]
            Target::Local(sa) => {
                if let Some(path) = sa.as_pathname().and_then(std::path::Path::to_str) {
                    f.write_str(path)
                } else {
                    f.write_str("[unnamed]")
                }
            }
        }
    }
}

/// Remote part of [`Target`]
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    /// Target scheme
    pub scheme: Scheme,
    /// Target host descriptor
    pub host: RemoteHost,
    /// Target port
    pub port: u16,
}

impl fmt::Display for RemoteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.scheme.as_str())?;
        f.write_str("://")?;
        write!(f, "{}", self.host)?;
        if !is_default_port(&self.scheme, self.port) {
            write!(f, ":{}", self.port)?;
        }
        Ok(())
    }
}

/// Remote address of [`RemoteTarget`]
#[derive(Clone, Debug)]
pub enum RemoteHost {
    /// Ip address
    Ip(IpAddr),
    /// Service name, usually a domain name
    Name(FastStr),
}

impl fmt::Display for RemoteHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ip(ip) => {
                if ip.is_ipv4() {
                    write!(f, "{ip}")
                } else {
                    write!(f, "[{ip}]")
                }
            }
            Self::Name(name) => f.write_str(name),
        }
    }
}

fn check_scheme(scheme: &Scheme) -> Result<()> {
    if scheme == &Scheme::HTTPS {
        #[cfg(not(feature = "__tls"))]
        {
            tracing::error!("[Volo-HTTP] https is not allowed when feature `tls` is not enabled");
            return Err(bad_scheme(scheme.clone()));
        }
        #[cfg(feature = "__tls")]
        return Ok(());
    }
    if scheme == &Scheme::HTTP {
        return Ok(());
    }
    tracing::error!("[Volo-HTTP] scheme '{scheme}' is unsupported");
    Err(bad_scheme(scheme.clone()))
}

impl Target {
    /// Create a [`Target`] by a scheme, host and port without checking scheme
    ///
    /// # Safety
    ///
    /// Users must ensure that the scheme is valid.
    ///
    /// - HTTP is always valid
    /// - HTTPS is valid if any feature of tls is enabled
    /// - Other schemes are always invalid
    pub const unsafe fn new_host_unchecked(scheme: Scheme, host: FastStr, port: u16) -> Self {
        Self::Remote(RemoteTarget {
            scheme,
            host: RemoteHost::Name(host),
            port,
        })
    }

    /// Create a [`Target`] by a scheme, ip address and port without checking scheme
    ///
    /// # Safety
    ///
    /// Users must ensure that the scheme is valid.
    ///
    /// - HTTP is always valid
    /// - HTTPS is valid if any feature of tls is enabled
    /// - Other schemes are always invalid
    pub const unsafe fn new_addr_unchecked(scheme: Scheme, ip: IpAddr, port: u16) -> Self {
        Self::Remote(RemoteTarget {
            scheme,
            host: RemoteHost::Ip(ip),
            port,
        })
    }

    /// Create a [`Target`] through a scheme, host name and a port
    pub fn new_host<S>(scheme: Option<Scheme>, host: S, port: Option<u16>) -> Result<Self>
    where
        S: Into<Cow<'static, str>>,
    {
        let scheme = scheme.unwrap_or(Scheme::HTTP);
        check_scheme(&scheme)?;
        let host = FastStr::from(host.into());
        let port = match port {
            Some(p) => p,
            None => get_default_port(&scheme),
        };
        // SAFETY: we've checked scheme
        Ok(unsafe { Self::new_host_unchecked(scheme, host, port) })
    }

    /// Create a [`Target`] through a scheme, ip address and a port
    pub fn new_addr(scheme: Option<Scheme>, ip: IpAddr, port: Option<u16>) -> Result<Self> {
        let scheme = scheme.unwrap_or(Scheme::HTTP);
        check_scheme(&scheme)?;
        let port = match port {
            Some(p) => p,
            None => get_default_port(&scheme),
        };
        // SAFETY: we've checked scheme
        Ok(unsafe { Self::new_addr_unchecked(scheme, ip, port) })
    }

    /// Create a [`Target`] through a host name
    pub fn from_host<S>(host: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        let host = FastStr::from(host.into());
        // SAFETY: HTTP is always valid
        unsafe { Self::new_host_unchecked(Scheme::HTTP, host, consts::HTTP_DEFAULT_PORT) }
    }

    /// Create a [`Target`] from [`Uri`]
    pub fn from_uri(uri: &Uri) -> Result<Self> {
        let scheme = uri.scheme().cloned().unwrap_or(Scheme::HTTP);
        check_scheme(&scheme)?;
        let host = uri.host().ok_or_else(no_address)?;
        let port = match uri.port_u16() {
            Some(p) => p,
            None => get_default_port(&scheme),
        };

        // SAFETY: we've checked scheme
        Ok(unsafe {
            match host.parse::<IpAddr>() {
                Ok(ip) => Self::new_addr_unchecked(scheme, ip, port),
                Err(_) => {
                    Self::new_host_unchecked(scheme, FastStr::from_string(host.to_owned()), port)
                }
            }
        })
    }

    /// Set a new scheme to the [`Target`]
    ///
    /// Note that if the previous is default port of the previous scheme, the port will be also
    /// updated to default port of the new scheme.
    pub fn set_scheme(&mut self, scheme: Scheme) -> Result<()> {
        let rt = match self.remote_mut() {
            Some(rt) => rt,
            None => {
                tracing::warn!("[Volo-HTTP] set scheme to an empty target or uds is invalid");
                return Err(scheme_unavailable());
            }
        };
        check_scheme(&scheme)?;
        if is_default_port(&rt.scheme, rt.port) {
            rt.port = get_default_port(&scheme);
        }
        rt.scheme = scheme;
        Ok(())
    }

    /// Set a new port to the [`Target`]
    pub fn set_port(&mut self, port: u16) -> Result<()> {
        let rt = match self.remote_mut() {
            Some(rt) => rt,
            None => {
                tracing::warn!("[Volo-HTTP] set port to an empty target or uds is invalid");
                return Err(port_unavailable());
            }
        };
        rt.port = port;
        Ok(())
    }

    /// Return if the [`Target`] is [`Target::None`]
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

    /// Return the remote [`IpAddr`] if the [`Target`] is an IP address.
    pub fn remote_ip(&self) -> Option<&IpAddr> {
        if let Self::Remote(rt) = &self {
            if let RemoteHost::Ip(ip) = &rt.host {
                return Some(ip);
            }
        }
        None
    }

    /// Return the remote host name if the [`Target`] is a host name.
    pub fn remote_host(&self) -> Option<&FastStr> {
        if let Self::Remote(rt) = &self {
            if let RemoteHost::Name(name) = &rt.host {
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

    /// Return target scheme if the [`Target`] is a remote address
    pub fn scheme(&self) -> Option<&Scheme> {
        if let Self::Remote(rt) = self {
            Some(&rt.scheme)
        } else {
            None
        }
    }

    /// Return target port if the [`Target`] is a remote address
    pub fn port(&self) -> Option<u16> {
        if let Self::Remote(rt) = self {
            Some(rt.port)
        } else {
            None
        }
    }
}

impl From<Address> for Target {
    fn from(value: Address) -> Self {
        match value {
            Address::Ip(sa) => {
                // SAFETY: HTTP is always valid
                unsafe { Target::new_addr_unchecked(Scheme::HTTP, sa.ip(), sa.port()) }
            }
            #[cfg(target_family = "unix")]
            Address::Unix(uds) => Target::Local(uds),
        }
    }
}

impl Apply<ClientContext> for Target {
    type Error = ClientError;

    fn apply(self, cx: &mut ClientContext) -> Result<(), Self::Error> {
        cx.set_target(self.clone());

        match self {
            Self::Remote(rt) => {
                match rt.host {
                    RemoteHost::Ip(ip) => {
                        let sa = SocketAddr::new(ip, rt.port);
                        tracing::trace!("[Volo-HTTP] Target::apply: set target to {sa}");
                        let callee = cx.rpc_info_mut().callee_mut();
                        callee.set_service_name(FastStr::from_string(format!("{}", sa.ip())));
                        callee.set_address(Address::Ip(sa));
                    }
                    RemoteHost::Name(host) => {
                        let port = rt.port;
                        tracing::trace!("[Volo-HTTP] Target::apply: set target to {host}:{port}");
                        let callee = cx.rpc_info_mut().callee_mut();
                        callee.set_service_name(host);
                        // Since Service Discover (DNS) can only access the `callee`, we must
                        // insert port into `callee` so that Service Discover can return the full
                        // address (IP with port) for transporting.
                        callee.insert(Port(port));
                    }
                }
            }
            #[cfg(target_family = "unix")]
            Self::Local(uds) => {
                let callee = cx.rpc_info_mut().callee_mut();
                callee.set_address(Address::Unix(uds));
                callee.set_service_name(FastStr::from_static_str("unix-domain-socket"));
            }
            Self::None => {}
        }

        Ok(())
    }
}
