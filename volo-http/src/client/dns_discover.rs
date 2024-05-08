use std::{net::SocketAddr, sync::Arc};

use async_broadcast::Receiver;
use faststr::FastStr;
use hickory_resolver::{AsyncResolver, TokioAsyncResolver};
use http::HeaderValue;
use lazy_static::lazy_static;
use volo::{
    context::Endpoint,
    discovery::{Change, Discover, Instance},
    loadbalance::error::LoadBalanceError,
    net::Address,
};

use super::HttpsTag;
use crate::{
    error::client::{bad_host_name, no_address},
    utils::consts::{HTTPS_DEFAULT_PORT, HTTP_DEFAULT_PORT},
};

lazy_static! {
    static ref RESOLVER: TokioAsyncResolver =
        AsyncResolver::tokio_from_system_conf().expect("failed to init dns resolver");
}

pub struct Port(pub u16);

pub struct DnsResolver;

impl DnsResolver {
    pub async fn resolve(host: &str, port: u16) -> Option<Address> {
        // Note that the Resolver will try to parse the host as an IP address first, so we don't
        // need to parse it manually.
        let mut iter = RESOLVER.lookup_ip(host).await.ok()?.into_iter();
        Some(Address::Ip(SocketAddr::new(iter.next()?, port)))
    }
}

impl Discover for DnsResolver {
    type Key = FastStr;
    type Error = LoadBalanceError;

    async fn discover<'s>(
        &'s self,
        endpoint: &'s Endpoint,
    ) -> Result<Vec<Arc<Instance>>, Self::Error> {
        if endpoint.service_name_ref().is_empty() && endpoint.address().is_none() {
            return Err(LoadBalanceError::Discover(Box::new(no_address())));
        }
        if let Some(address) = endpoint.address() {
            let instance = Instance {
                address,
                weight: 10,
                tags: Default::default(),
            };
            return Ok(vec![Arc::new(instance)]);
        }
        let port = if let Some(port) = endpoint.tags.get::<Port>() {
            port.0
        } else if cfg!(feature = "__tls") && endpoint.tags.contains::<HttpsTag>() {
            HTTPS_DEFAULT_PORT
        } else {
            HTTP_DEFAULT_PORT
        };

        if let Some(address) = Self::resolve(endpoint.service_name_ref(), port).await {
            let instance = Instance {
                address,
                weight: 10,
                tags: Default::default(),
            };
            return Ok(vec![Arc::new(instance)]);
        };
        Err(LoadBalanceError::Discover(Box::new(bad_host_name())))
    }

    fn key(&self, endpoint: &Endpoint) -> Self::Key {
        endpoint.service_name()
    }

    fn watch(&self, _: Option<&[Self::Key]>) -> Option<Receiver<Change<Self::Key>>> {
        None
    }
}

#[derive(Clone, Default)]
pub enum Target {
    #[default]
    None,
    Address {
        addr: Address,
        #[cfg(feature = "__tls")]
        https: bool,
    },
    Host {
        #[cfg(feature = "__tls")]
        https: bool,
        host: FastStr,
        port: Option<u16>,
    },
}

impl Target {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn is_https(&self) -> bool {
        match self {
            Self::None => false,
            Self::Address { https, .. } => *https,
            Self::Host { https, .. } => *https,
        }
    }

    pub fn address(&self) -> Option<&Address> {
        match self {
            Self::Address { addr, .. } => Some(addr),
            _ => None,
        }
    }

    pub fn port(&self) -> Option<u16> {
        match self {
            Self::Host { port, .. } => *port,
            _ => None,
        }
    }

    pub fn name(&self) -> FastStr {
        match self {
            Self::None => FastStr::empty(),
            // The `name` is callee name and used for service discover. If there is an address, the
            // callee name will only be used .
            Self::Address { .. } => FastStr::empty(),
            // For DNS resolver, we should keep only host name.
            Self::Host { host, .. } => host.clone(),
        }
    }

    pub fn host(&self) -> Option<HeaderValue> {
        match self {
            Self::None => None,
            Self::Address {
                addr,
                #[cfg(feature = "__tls")]
                https,
            } => {
                if let Address::Ip(socket) = addr {
                    let port = socket.port();
                    // If the port is default port, just ignore it.
                    #[cfg(feature = "__tls")]
                    if *https && port == HTTPS_DEFAULT_PORT {
                        return HeaderValue::try_from(socket.ip().to_string()).ok();
                    }
                    if port == HTTP_DEFAULT_PORT {
                        return HeaderValue::try_from(socket.ip().to_string()).ok();
                    }
                    return HeaderValue::try_from(socket.to_string()).ok();
                }
                None
            }
            Self::Host {
                #[cfg(feature = "__tls")]
                https,
                host,
                port,
            } => {
                if let Some(port) = port {
                    #[cfg(feature = "__tls")]
                    if *https && *port != HTTPS_DEFAULT_PORT {
                        return HeaderValue::try_from(format!("{host}:{port}")).ok();
                    }
                    if *port != HTTP_DEFAULT_PORT {
                        return HeaderValue::try_from(format!("{host}:{port}")).ok();
                    }
                }
                HeaderValue::from_str(host.as_str()).ok()
            }
        }
    }
}
