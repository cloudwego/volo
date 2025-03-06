//! HTTP target address related types
//!
//! See [`Target`], [`RemoteTarget`] for more details.

use std::{
    borrow::Cow,
    net::{IpAddr, SocketAddr},
};

use faststr::FastStr;
use http::uri::{Scheme, Uri};
use volo::{client::Apply, context::Context, net::Address};

use super::dns::Port;
use crate::{
    context::ClientContext,
    error::{
        client::{bad_address, bad_scheme, no_address, Result},
        ClientError,
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

/// Remote part of [`Target`]
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    /// Target scheme
    pub scheme: Scheme,
    /// The target address
    pub addr: RemoteTargetAddress,
    /// Target port
    pub port: u16,
}

/// Remote address of [`RemoteTarget`]
#[derive(Clone, Debug)]
pub enum RemoteTargetAddress {
    /// Ip address
    Ip(IpAddr),
    /// Service name, usually a domain name
    Name(FastStr),
}

#[allow(clippy::result_large_err)]
fn check_scheme(scheme: &Scheme) -> Result<()> {
    if scheme == &Scheme::HTTPS {
        #[cfg(not(feature = "__tls"))]
        {
            tracing::error!("[Volo-HTTP] https is not allowed when feature `tls` is not enabled");
            return Err(bad_scheme());
        }
        #[cfg(feature = "__tls")]
        return Ok(());
    }
    if scheme == &Scheme::HTTP {
        return Ok(());
    }
    tracing::error!("[Volo-HTTP] scheme '{scheme}' is unsupported");
    Err(bad_scheme())
}

fn get_default_port(scheme: &Scheme) -> u16 {
    #[cfg(feature = "__tls")]
    if scheme == &Scheme::HTTPS {
        return consts::HTTPS_DEFAULT_PORT;
    }
    if scheme == &Scheme::HTTP {
        return consts::HTTP_DEFAULT_PORT;
    }
    unreachable!("[Volo-HTTP] https is not allowed when feature `tls` is not enabled")
}

pub(super) fn is_default_port(scheme: &Scheme, port: u16) -> bool {
    get_default_port(scheme) == port
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
            addr: RemoteTargetAddress::Name(host),
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
            addr: RemoteTargetAddress::Ip(ip),
            port,
        })
    }

    /// Create a [`Target`] through a scheme, host name and a port
    #[allow(clippy::result_large_err)]
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
    #[allow(clippy::result_large_err)]
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
    #[allow(clippy::result_large_err)]
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
            match host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
            {
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
    #[allow(clippy::result_large_err)]
    pub fn set_scheme(&mut self, scheme: Scheme) -> Result<()> {
        let rt = match self.remote_mut() {
            Some(rt) => rt,
            None => {
                tracing::warn!("[Volo-HTTP] set scheme to an empty target or uds is invalid");
                return Err(bad_address());
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
    #[allow(clippy::result_large_err)]
    pub fn set_port(&mut self, port: u16) -> Result<()> {
        let rt = match self.remote_mut() {
            Some(rt) => rt,
            None => {
                tracing::warn!("[Volo-HTTP] set port to an empty target or uds is invalid");
                return Err(bad_address());
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
        if self.is_none() {
            return Ok(());
        }

        let callee = cx.rpc_info_mut().callee_mut();
        if !(callee.service_name_ref().is_empty() && callee.address.is_none()) {
            // Target exists in context
            return Ok(());
        }

        match self {
            Self::Remote(rt) => {
                callee.insert(rt.scheme);
                match rt.addr {
                    RemoteTargetAddress::Ip(ip) => {
                        let sa = SocketAddr::new(ip, rt.port);
                        tracing::trace!("[Volo-HTTP] Target::apply: set target to {sa}");
                        callee.set_address(Address::Ip(sa));
                    }
                    RemoteTargetAddress::Name(host) => {
                        let port = rt.port;
                        tracing::trace!("[Volo-HTTP] Target::apply: set target to {host}:{port}");
                        callee.set_service_name(host);
                        callee.insert(Port(port));
                    }
                }
            }
            #[cfg(target_family = "unix")]
            Self::Local(uds) => {
                callee.set_address(Address::Unix(uds));
            }
            Self::None => {
                unreachable!()
            }
        }

        Ok(())
    }
}
