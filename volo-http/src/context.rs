use hyper::{
    header::HeaderValue,
    http::{
        header,
        uri::{Authority, Scheme},
        Extensions, HeaderMap, HeaderName, Method, Uri, Version,
    },
};
use url::{Host, Url};
use volo::net::Address;

use crate::param::Params;

static X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
static X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

#[derive(Debug)]
pub struct HttpContext {
    pub peer: Address,
    pub method: Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap,
    pub extensions: Extensions,

    pub params: Params,
}

#[derive(Debug)]
pub struct ConnectionInfo {
    scheme: Scheme,
    host: Option<Host>,
    port: Option<u16>,
}

impl HttpContext {
    pub(crate) fn get_connection_info(&self) -> ConnectionInfo {
        let mut host = None;
        let mut scheme = None;

        for (name, val) in self
            .headers
            .get_all(&header::FORWARDED)
            .into_iter()
            .filter_map(|hdr| hdr.to_str().ok())
            // "for=1.2.3.4, for=5.6.7.8; scheme=https"
            .flat_map(|val| val.split(';'))
            // ["for=1.2.3.4, for=5.6.7.8", " scheme=https"]
            .flat_map(|vals| vals.split(','))
            // ["for=1.2.3.4", " for=5.6.7.8", " scheme=https"]
            .flat_map(|pair| {
                let mut items = pair.trim().splitn(2, '=');
                Some((items.next()?, items.next()?))
            })
        {
            // [(name , val      ), ...                                    ]
            // [("for", "1.2.3.4"), ("for", "5.6.7.8"), ("scheme", "https")]

            // taking the first value for each property is correct because spec states that first
            // "for" value is client and rest are proxies; multiple values other properties have
            // no defined semantics
            //
            // > In a chain of proxy servers where this is fully utilized, the first
            // > "for" parameter will disclose the client where the request was first
            // > made, followed by any subsequent proxy identifiers.
            // --- https://datatracker.ietf.org/doc/html/rfc7239#section-5.2

            match name.trim().to_lowercase().as_str() {
                "host" => host.get_or_insert_with(|| unquote(val)),
                "proto" => scheme.get_or_insert_with(|| unquote(val)),
                "by" => {
                    // TODO: implement https://datatracker.ietf.org/doc/html/rfc7239#section-5.1
                    continue;
                }
                _ => continue,
            };
        }

        let host = match host {
            // Forwarded
            Some(host) => host,
            None => {
                if let Some(host) = first_header_value(&self.headers, &X_FORWARDED_HOST) {
                    // X-Forwarded-Host
                    host
                } else if let Some(Ok(host)) =
                    self.headers.get(&header::HOST).map(HeaderValue::to_str)
                {
                    // Host
                    host
                } else if let Some(host) = self.uri.authority().map(Authority::as_str) {
                    host
                } else {
                    ""
                }
            }
        };
        let host = host.to_owned();

        let scheme = match scheme {
            // Forwarded
            Some(scheme) => Some(scheme),
            None => {
                // X-Forwarded-Host
                first_header_value(&self.headers, &X_FORWARDED_PROTO)
            }
        };
        // map str to `Scheme`
        let scheme = match scheme {
            Some(scheme) => Scheme::try_from(scheme).ok(),
            None => self.uri.scheme().map(Scheme::to_owned),
        };
        // fallback
        let scheme = match scheme {
            Some(scheme) => scheme,
            None => Scheme::HTTP,
        };

        let (host, port) = match Url::parse(format!("{scheme}://{host}/").as_str()) {
            // SAFETY: calling `unwrap` is safe because the original string is a valid url
            // constructed with the format `scheme://host/`
            Ok(url) => (url.host().map(|s| s.to_owned()), url.port()),
            Err(_) => (None, None),
        };

        ConnectionInfo { host, port, scheme }
    }
}

impl ConnectionInfo {
    /// Hostname and port of the request.
    ///
    /// Hostname is resolved through the following, in order:
    /// - `Forwarded` header
    /// - `X-Forwarded-Host` header
    /// - `Host` header
    /// - request target / URI
    #[inline]
    pub fn hostport(&self) -> (Option<&Host>, Option<u16>) {
        (self.host.as_ref(), self.port)
    }

    /// Scheme of the request.
    ///
    /// Scheme is resolved through the following, in order:
    /// - `Forwarded` header
    /// - `X-Forwarded-Proto` header
    /// - request target / URI
    #[inline]
    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }
}

fn unquote(val: &str) -> &str {
    val.trim().trim_start_matches('"').trim_end_matches('"')
}

fn first_header_value<'a>(headers: &'a HeaderMap, name: &'_ HeaderName) -> Option<&'a str> {
    let hdr = headers.get(name)?.to_str().ok()?;
    let val = hdr.split(',').next()?.trim();
    Some(val)
}
