use std::net::{IpAddr, SocketAddr};

use faststr::FastStr;
use http::{uri::Scheme, Uri};
use tokio::net::lookup_host;
use volo::net::Address;

use crate::error::client::{bad_scheme, builder_error, uri_without_host, Result};

const HTTP_DEFAULT_PORT: u16 = 80;
const HTTPS_DEFAULT_PORT: u16 = 443;

pub trait IntoUri {
    fn into_uri(self) -> Result<Uri>;
}

impl IntoUri for Uri {
    fn into_uri(self) -> Result<Uri> {
        if self.scheme().is_none() {
            Err(uri_without_host())
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
                    return Err(bad_scheme());
                }
            };
            // `match` is unavailable here, ref:
            // https://doc.rust-lang.org/stable/std/marker/trait.StructuralPartialEq.html
            if scheme == &Scheme::HTTP {
                HTTP_DEFAULT_PORT
            } else if scheme == &Scheme::HTTPS {
                HTTPS_DEFAULT_PORT
            } else {
                return Err(bad_scheme());
            }
        }
    };
    Ok(port)
}

pub fn parse_address(uri: &Uri) -> Result<Address> {
    let host = uri.host().ok_or(uri_without_host())?;
    let port = get_port(uri)?;
    match host.parse::<IpAddr>() {
        Ok(addr) => Ok(Address::from(SocketAddr::new(addr, port))),
        Err(e) => Err(builder_error(e)),
    }
}

pub async fn resolve(uri: &Uri) -> Result<impl Iterator<Item = Address>> {
    let host = uri.host().ok_or_else(uri_without_host)?.to_owned();
    let port = get_port(uri)?;
    match lookup_host((host, port)).await {
        Ok(addrs) => Ok(addrs.map(Address::from)),
        Err(e) => Err(builder_error(e)),
    }
}
