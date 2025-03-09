use std::str::FromStr;

use anyhow::anyhow;
use url::{Host, Url};


#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RpcEndpoint {
    pub host: Host,
    pub port: u16,
    pub server_name: Option<String>,
    pub tls: bool,
}

impl RpcEndpoint {
    pub fn parse(s: &str) -> Result<RpcEndpoint, anyhow::Error> {
        let u = Url::parse(s)?;
        let host = match u.host().ok_or_else(|| anyhow!("missing host"))? {
            Host::Domain(domain) => Host::Domain(domain.to_string()),
            Host::Ipv4(ip) => Host::Ipv4(ip),
            Host::Ipv6(ip) => Host::Ipv6(ip),
        };

        let port = u
            .port_or_known_default()
            .ok_or_else(|| anyhow!("unknown schema for port"))?;
        let server_name = if let Host::Domain(ref server_name) = host {
            Some(server_name.clone())
        } else {
            None
        };

        let tls = ["https", "tls", "xds"].contains(&u.scheme());

        Ok(RpcEndpoint { host, port, server_name, tls })
    }

    pub fn uri(&self) -> http::Uri {
        let scheme = if self.tls { "https" } else { "http" };
        let authority = match (scheme, self.port) {
            ("https", 443) | ("http", 80) => self.host.to_string(),
            _ => format!("{}:{}", self.host.to_string(), self.port),
        };
        http::Uri::builder()
            .scheme(scheme)
            .authority(authority)
            .path_and_query("/")
            .build()
            .expect("rpc endpoint uri build")
    }
}

impl FromStr for RpcEndpoint {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}
