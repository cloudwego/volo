pub mod conn;
pub mod dial;
pub mod incoming;
mod probe;

use std::{borrow::Cow, fmt, net::Ipv6Addr, path::Path};

pub use incoming::{Incoming, MakeIncoming};

#[cfg(not(target_os = "windows"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Address {
    Ip(std::net::SocketAddr),
    Unix(Cow<'static, Path>),
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Address {
    Ip(std::net::SocketAddr),
}

#[cfg(target_os = "windows")]
impl Address {
    pub fn favor_dual_stack(self) -> Self {
        match self {
            Address::Ip(addr) => {
                if addr.ip().is_unspecified() && should_favor_ipv6() {
                    Address::Ip((Ipv6Addr::UNSPECIFIED, addr.port()).into())
                } else {
                    self
                }
            }
        }

    }
}

#[cfg(not(target_os = "windows"))]
impl Address {
    pub fn favor_dual_stack(self) -> Self {
        match self {
            Address::Ip(addr) => {
                if addr.ip().is_unspecified() && should_favor_ipv6() {
                    Address::Ip((Ipv6Addr::UNSPECIFIED, addr.port()).into())
                } else {
                    self
                }
            }
            _ => self,
        }
    }
}

fn should_favor_ipv6() -> bool {
    let probed = probe::probe();
    !probed.ipv4 || probed.ipv4_mapped_ipv6
}


#[cfg(target_os = "windows")]
impl fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Address::Ip(addr) => write!(f, "{}", addr),
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Address::Ip(addr) => write!(f, "{}", addr),
            Address::Unix(_) => write!(f, "-"),
        }
    }
}

impl From<std::net::SocketAddr> for Address {
    fn from(addr: std::net::SocketAddr) -> Self {
        Address::Ip(addr)
    }
}

#[cfg(not(target_os = "windows"))]
impl From<Cow<'static, Path>> for Address {
    fn from(addr: Cow<'static, Path>) -> Self {
        Address::Unix(addr)
    }
}

#[cfg(not(target_os = "windows"))]
impl TryFrom<tokio::net::unix::SocketAddr> for Address {
    type Error = std::io::Error;

    fn try_from(value: tokio::net::unix::SocketAddr) -> Result<Self, Self::Error> {
        Ok(Address::Unix(Cow::Owned(
            value
                .as_pathname()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "unix socket doesn't have an address",
                    )
                })?
                .to_owned(),
        )))
    }
}
