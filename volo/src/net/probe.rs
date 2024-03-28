use lazy_static::lazy_static;
use socket2::{Domain, Protocol, Socket, Type};

#[derive(Debug)]
pub struct IpStackCapability {
    pub ipv4: bool,
    #[allow(dead_code)]
    pub ipv6: bool,
    pub ipv4_mapped_ipv6: bool,
}

impl IpStackCapability {
    fn probe() -> Self {
        IpStackCapability {
            ipv4: Self::probe_ipv4(),
            ipv6: Self::probe_ipv6(),
            ipv4_mapped_ipv6: Self::probe_ipv4_mapped_ipv6(),
        }
    }

    fn probe_ipv4() -> bool {
        let s = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP));
        s.is_ok()
    }

    fn probe_ipv6() -> bool {
        let s = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP));
        let s = match s {
            Ok(s) => s,
            Err(_) => return false,
        };
        // this error is ignored in go, follow their strategy
        let _ = s.set_only_v6(true);
        let addr: std::net::SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], 0).into();
        s.bind(&addr.into()).is_ok()
    }

    fn probe_ipv4_mapped_ipv6() -> bool {
        let s = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP));
        let s = match s {
            Ok(s) => s,
            Err(_) => return false,
        };
        !s.only_v6().unwrap_or(true)
    }
}

pub fn probe() -> &'static IpStackCapability {
    lazy_static! {
        static ref CAPABILITY: IpStackCapability = IpStackCapability::probe();
    }
    &CAPABILITY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn tryout_probe() {
        println!("{:?}", probe());
    }
}
