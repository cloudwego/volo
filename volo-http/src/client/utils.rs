use std::net::SocketAddr;

use faststr::FastStr;
use hickory_resolver::{AsyncResolver, Resolver, TokioAsyncResolver};
use http::uri::{Scheme, Uri};
use lazy_static::lazy_static;
use volo::net::Address;

use super::ClientInner;
use crate::{
    context::client::CalleeName,
    error::client::{bad_host_name, bad_scheme, no_address, unreachable_builder_error, Result},
    utils::consts,
};

lazy_static! {
    static ref SYNC_RESOLVER: Resolver =
        Resolver::from_system_conf().expect("failed to init dns resolver");
    static ref ASYNC_RESOLVER: TokioAsyncResolver =
        AsyncResolver::tokio_from_system_conf().expect("failed to init dns resolver");
}

#[derive(Clone)]
pub struct Target {
    pub addr: Address,
    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub use_tls: bool,
    pub(crate) callee_name: FastStr,
}

#[derive(Clone, Default)]
pub enum TargetBuilder {
    #[default]
    None,
    Address {
        addr: Address,
        #[cfg(feature = "__tls")]
        use_tls: bool,
    },
    Host {
        scheme: Option<Scheme>,
        host: FastStr,
        port: Option<u16>,
    },
}

impl TargetBuilder {
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    pub fn is_tls(&self) -> bool {
        match self {
            Self::None => false,
            Self::Address { use_tls, .. } => *use_tls,
            Self::Host { scheme, .. } => {
                // If scheme is none, use https by default
                scheme.as_ref() == Some(&Scheme::HTTPS) || scheme.is_none()
            }
        }
    }

    pub(crate) fn gen_callee_name(&self, mode: &CalleeName, orig_callee_name: &FastStr) -> FastStr {
        match *mode {
            CalleeName::None => FastStr::empty(),
            CalleeName::TargetName => match self {
                Self::None => FastStr::empty(),
                Self::Address { addr, .. } => FastStr::from_string(format!("{addr}")),
                Self::Host {
                    ref scheme,
                    host,
                    port,
                } => match port {
                    Some(port) => {
                        if get_port(scheme.as_ref()) == Some(*port) {
                            host.clone()
                        } else {
                            FastStr::from_string(format!("{host}:{port}"))
                        }
                    }
                    None => host.clone(),
                },
            },
            CalleeName::OriginalCalleeName => orig_callee_name.clone(),
        }
    }

    pub fn resolve_sync(self) -> Result<Address> {
        match self {
            Self::None => Err(no_address()),
            Self::Address { addr, .. } => Ok(addr),
            Self::Host { scheme, host, port } => resolve_sync(scheme, &host, port),
        }
    }

    pub async fn resolve(self) -> Result<Address> {
        match self {
            Self::None => Err(no_address()),
            Self::Address { addr, .. } => Ok(addr),
            Self::Host { scheme, host, port } => resolve(scheme, &host, port).await,
        }
    }

    pub(crate) async fn into_target(self, client_inner: &ClientInner) -> Result<Option<Target>> {
        if matches!(self, Self::None) {
            return Ok(None);
        }
        let callee_name = self.gen_callee_name(
            &client_inner.callee_name_mode,
            &client_inner.default_callee_name,
        );
        #[cfg(feature = "__tls")]
        let use_tls = self.is_tls();
        Ok(Some(Target {
            addr: self.resolve().await?,
            #[cfg(feature = "__tls")]
            use_tls,
            callee_name,
        }))
    }
}

fn get_port(scheme: Option<&Scheme>) -> Option<u16> {
    // `match` is unavailable here, ref:
    // https://doc.rust-lang.org/stable/std/marker/trait.StructuralPartialEq.html
    #[cfg(feature = "__tls")]
    if scheme == Some(&Scheme::HTTPS) || scheme.is_none() {
        return Some(consts::HTTPS_DEFAULT_PORT);
    }
    if scheme == Some(&Scheme::HTTP) || scheme.is_none() {
        return Some(consts::HTTP_DEFAULT_PORT);
    }

    None
}

fn prepare_host_and_port(
    scheme: Option<Scheme>,
    host: &str,
    port: Option<u16>,
) -> Result<(&str, u16)> {
    // Trim the brackets from the host name if it's an IPv6 address.
    //
    // e.g., for `http://[::1]:8080/`, it can be trimed to `::1` rather than `[::1]`
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = match port {
        Some(port) => port,
        None => get_port(scheme.as_ref()).ok_or_else(|| {
            if let Some(scheme) = scheme {
                if let Ok(uri) = Uri::try_from(format!("{}://{}", scheme, host)) {
                    return bad_scheme(uri);
                }
            }
            unreachable_builder_error()
        })?,
    };

    Ok((host, port))
}

fn resolve_sync(scheme: Option<Scheme>, host: &str, port: Option<u16>) -> Result<Address> {
    let (host, port) = prepare_host_and_port(scheme, host, port)?;

    // The Resolver will try to parse the host as an IP address first, so we don't need
    // to parse it manually.
    if let Ok(resp) = SYNC_RESOLVER.lookup_ip(host) {
        if let Some(addr) = resp.iter().next() {
            return Ok(Address::Ip(SocketAddr::new(addr, port)));
        }
    };

    Err(bad_host_name(
        Uri::try_from(host).map_err(|_| unreachable_builder_error())?,
    ))
}

async fn resolve(scheme: Option<Scheme>, host: &str, port: Option<u16>) -> Result<Address> {
    let (host, port) = prepare_host_and_port(scheme, host, port)?;

    // The address may be a domain name, so we need to resolve it.
    //
    // Note that the Resolver will try to parse the host as an IP address first, so we don't need
    // to parse it manually.
    if let Ok(resp) = ASYNC_RESOLVER.lookup_ip(host).await {
        if let Some(addr) = resp.iter().next() {
            return Ok(Address::Ip(SocketAddr::new(addr, port)));
        }
    };

    Err(bad_host_name(
        Uri::try_from(host).map_err(|_| unreachable_builder_error())?,
    ))
}
