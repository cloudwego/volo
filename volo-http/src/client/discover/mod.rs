use std::net::{IpAddr, SocketAddr};

use faststr::FastStr;
use http::{uri::Scheme, HeaderValue, Uri};
use metainfo::{FastStrMap, TypeMap};
use volo::{context::Endpoint, net::Address};

use crate::{
    error::{client::bad_scheme, ClientError},
    utils::consts::{HTTPS_DEFAULT_PORT, HTTP_DEFAULT_PORT},
};

pub(crate) mod dns;
pub use self::dns::Port;

/// Function for parsing `Target` and updating `Endpoint`.
///
/// The `TargetParser` usually used for service discover. It can update `Endpoint` from `Target`,
/// and the service discover will resolve the `Endpoint` to `Address`(es) and access them.
pub type TargetParser = fn(&Target, &mut Endpoint);

#[derive(Default)]
pub struct Target {
    inner: TargetInner,
    #[cfg(feature = "__tls")]
    https: bool,

    faststr_tags: FastStrMap,
    tags: TypeMap,
}

#[derive(Default)]
pub enum TargetInner {
    #[default]
    None,
    Address(Address),
    Host {
        host: FastStr,
        port: u16,
    },
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
        let port = match uri.port_u16() {
            Some(port) => port,
            None => {
                if https {
                    HTTPS_DEFAULT_PORT
                } else {
                    HTTP_DEFAULT_PORT
                }
            }
        };

        let inner = match host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
        {
            Ok(addr) => TargetInner::Address(Address::Ip(SocketAddr::new(addr, port))),
            Err(_) => TargetInner::Host {
                host: FastStr::from_string(host.to_owned()),
                port,
            },
        };

        Some(Ok(Self {
            inner,
            #[cfg(feature = "__tls")]
            https,
            ..Default::default()
        }))
    }

    /// Build a `Target` from an address.
    pub fn from_address<A>(
        address: A,
        #[cfg(feature = "__tls")]
        #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
        https: bool,
    ) -> Self
    where
        A: Into<Address>,
    {
        Self {
            inner: TargetInner::Address(address.into()),
            #[cfg(feature = "__tls")]
            https,
            ..Default::default()
        }
    }

    /// Build a `Target` from a host name and port.
    ///
    /// Note that the `host` must be a host name, it will be used for service discover.
    ///
    /// It should NOT be an address or something with port.
    ///
    /// If you have a uri and you are not sure if the host is a host, try `from_uri`.
    pub fn from_host<S>(
        host: S,
        port: Option<u16>,
        #[cfg(feature = "__tls")]
        #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
        https: bool,
    ) -> Self
    where
        S: AsRef<str>,
    {
        let port = match port {
            Some(p) => p,
            None => {
                #[cfg(feature = "__tls")]
                if https {
                    HTTPS_DEFAULT_PORT
                } else {
                    HTTP_DEFAULT_PORT
                }
                #[cfg(not(feature = "__tls"))]
                HTTP_DEFAULT_PORT
            }
        };
        Self {
            inner: TargetInner::Host {
                host: FastStr::from_string(String::from(host.as_ref())),
                port,
            },
            #[cfg(feature = "__tls")]
            https,
            ..Default::default()
        }
    }

    /// Get the target repr.
    pub fn inner(&self) -> &TargetInner {
        &self.inner
    }

    /// Return if the `Target` is `None`.
    pub fn is_none(&self) -> bool {
        matches!(self.inner, TargetInner::None)
    }

    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn set_https(&mut self, https: bool) {
        self.https = https;
    }

    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn is_https(&self) -> bool {
        if self.is_none() {
            false
        } else {
            self.https
        }
    }

    /// Return the `Address` if the `Target` is an address.
    pub fn address(&self) -> Option<&Address> {
        match &self.inner {
            TargetInner::Address(addr) => Some(addr),
            _ => None,
        }
    }

    /// Return the host name if the `Target` is a host name.
    pub fn host(&self) -> Option<&FastStr> {
        match &self.inner {
            TargetInner::Host { host, .. } => Some(host),
            _ => None,
        }
    }

    /// Return the port if the `Target` is a host name.
    pub fn port(&self) -> Option<u16> {
        match &self.inner {
            &TargetInner::Host { port, .. } => Some(port),
            _ => None,
        }
    }

    /// Insert a tag into this `Target`.
    #[inline]
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.tags.insert(val);
    }

    /// Check if the tag exists.
    #[inline]
    pub fn contains<T: 'static>(&self) -> bool {
        self.tags.contains::<T>()
    }

    /// Get a reference to a tag previously inserted on this `Target`.
    #[inline]
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.tags.get::<T>()
    }

    /// Remove a tag if it exists and return it.
    #[inline]
    pub fn remove<T: 'static>(&mut self) -> Option<T> {
        self.tags.remove::<T>()
    }

    /// Insert a tag into this `Target`.
    #[inline]
    pub fn insert_faststr<T: Send + Sync + 'static>(&mut self, val: FastStr) {
        self.faststr_tags.insert::<T>(val);
    }

    /// Check if the tag exists.
    #[inline]
    pub fn contains_faststr<T: 'static>(&self) -> bool {
        self.faststr_tags.contains::<T>()
    }

    /// Get a reference to a tag previously inserted on this `Target`.
    #[inline]
    pub fn get_faststr<T: 'static>(&self) -> Option<&FastStr> {
        self.faststr_tags.get::<T>()
    }

    /// Remove a tag if it exists and return it.
    #[inline]
    pub fn remove_faststr<T: 'static>(&mut self) -> Option<FastStr> {
        self.faststr_tags.remove::<T>()
    }

    /// Generate a `HeaderValue` for `Host` in HTTP headers.
    pub fn gen_host(&self) -> Option<HeaderValue> {
        match &self.inner {
            TargetInner::None => None,
            TargetInner::Address(addr) => {
                #[allow(irrefutable_let_patterns)]
                if let Address::Ip(socket) = addr {
                    let port = socket.port();
                    // If the port is default port, just ignore it.
                    #[cfg(feature = "__tls")]
                    if self.https && port == HTTPS_DEFAULT_PORT {
                        return HeaderValue::try_from(addr_to_string(socket, false)).ok();
                    }
                    if port == HTTP_DEFAULT_PORT {
                        return HeaderValue::try_from(addr_to_string(socket, false)).ok();
                    }
                    return HeaderValue::try_from(addr_to_string(socket, true)).ok();
                }
                None
            }
            TargetInner::Host { host, port } => {
                #[cfg(feature = "__tls")]
                if self.https && *port != HTTPS_DEFAULT_PORT {
                    return HeaderValue::try_from(format!("{host}:{port}")).ok();
                }
                if *port != HTTP_DEFAULT_PORT {
                    return HeaderValue::try_from(format!("{host}:{port}")).ok();
                }
                HeaderValue::from_str(host.as_str()).ok()
            }
        }
    }
}

fn addr_to_string(addr: &SocketAddr, with_port: bool) -> String {
    if with_port {
        return addr.to_string();
    }
    let ip = addr.ip();
    if ip.is_ipv6() {
        format!("[{ip}]")
    } else {
        ip.to_string()
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
