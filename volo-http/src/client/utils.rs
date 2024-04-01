use std::net::{IpAddr, SocketAddr};

use faststr::FastStr;
use http::{uri::Scheme, Uri};
use tokio::net::lookup_host;
use volo::net::Address;

use crate::{
    error::client::{bad_host_name, bad_scheme, builder_error, uri_without_host, Result},
    utils::consts,
};

pub trait IntoUri {
    fn into_uri(self) -> Result<Uri>;
}

impl IntoUri for Uri {
    fn into_uri(self) -> Result<Uri> {
        if self.scheme().is_none() {
            Err(uri_without_host(self))
        } else {
            Ok(self)
        }
    }
}

impl<'a> IntoUri for &'a str {
    fn into_uri(self) -> Result<Uri> {
        match self.parse::<Uri>() {
            Ok(uri) => uri.into_uri(),
            Err(err) => Err(builder_error(err)),
        }
    }
}

impl IntoUri for String {
    fn into_uri(self) -> Result<Uri> {
        self.as_str().into_uri()
    }
}

impl IntoUri for FastStr {
    fn into_uri(self) -> Result<Uri> {
        self.as_str().into_uri()
    }
}

fn get_port(uri: &Uri) -> Result<u16> {
    let port = match uri.port_u16() {
        Some(port) => port,
        None => {
            let scheme = match uri.scheme() {
                Some(scheme) => scheme,
                None => {
                    return Err(bad_scheme(uri.to_owned()));
                }
            };
            // `match` is unavailable here, ref:
            // https://doc.rust-lang.org/stable/std/marker/trait.StructuralPartialEq.html
            #[cfg(feature = "__tls")]
            if scheme == &Scheme::HTTPS {
                return Ok(consts::HTTPS_DEFAULT_PORT);
            }
            if scheme == &Scheme::HTTP {
                consts::HTTP_DEFAULT_PORT
            } else {
                return Err(bad_scheme(uri.to_owned()));
            }
        }
    };
    Ok(port)
}

pub async fn resolve(uri: &Uri) -> Result<Address> {
    // Trim the brackets from the host name if it's an IPv6 address.
    //
    // e.g., for `http://[::1]:8080/`, it can be trimed to `::1` rather than `[::1]`
    let host = uri
        .host()
        .ok_or(uri_without_host(uri.to_owned()))?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = get_port(uri)?;

    // Parse the adddress directly.
    if let Ok(addr) = host.parse::<IpAddr>() {
        return Ok(Address::from(SocketAddr::new(addr, port)));
    }

    // The address may be a domain name, so we need to resolve it.
    let mut iter = lookup_host((host, port)).await.map_err(builder_error)?;
    let addr = iter.next().ok_or_else(|| bad_host_name(uri.to_owned()))?;
    Ok(Address::Ip(addr))
}
