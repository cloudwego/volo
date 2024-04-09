use http::{
    header,
    header::{HeaderMap, HeaderValue},
    request::Parts,
    uri::{Authority, PathAndQuery, Scheme, Uri},
};
use volo::{
    context::{Context, Reusable, Role, RpcCx, RpcInfo},
    net::Address,
    newtype_impl_context,
};

use crate::{
    server::param::UrlParamsVec,
    utils::{
        consts::{HTTPS_DEFAULT_PORT, HTTP_DEFAULT_PORT},
        macros::{impl_deref_and_deref_mut, impl_getter},
    },
};

#[derive(Debug)]
pub struct ServerContext(pub(crate) RpcCx<ServerCxInner, Config>);

impl ServerContext {
    pub fn new(peer: Address) -> Self {
        let mut cx = RpcCx::new(
            RpcInfo::<Config>::with_role(Role::Server),
            ServerCxInner {
                params: UrlParamsVec::default(),
            },
        );
        cx.rpc_info_mut().caller_mut().set_address(peer);
        Self(cx)
    }
}

impl_deref_and_deref_mut!(ServerContext, RpcCx<ServerCxInner, Config>, 0);

newtype_impl_context!(ServerContext, Config, 0);

#[derive(Clone, Debug)]
pub struct ServerCxInner {
    pub params: UrlParamsVec,
}

impl ServerCxInner {
    impl_getter!(params, UrlParamsVec);
}

#[derive(Clone, Debug, Default)]
pub struct Config {}

impl Reusable for Config {
    fn clear(&mut self) {}
}

pub trait RequestPartsExt {
    /// Parse `Forwarded` in headers.
    fn forwarded(&self) -> Forwarded;

    /// Get the URI in HTTP header of original request.
    ///
    /// For most cases, the uri is a path (and query) starting with `/`.
    fn request_uri(&self) -> Uri;

    /// Get the full URI including scheme, host, port (if any), and path.
    fn full_uri(&self) -> Option<Uri>;

    /// Get the scheme of the request URI.
    ///
    /// In fact, if the TLS is enabled, the scheme is always `https`, otherwise `http`.
    fn scheme(&self) -> Scheme;

    /// Get host name of the request URI from header `Host`.
    fn host(&self) -> Option<&str>;

    /// Get port of the request URI.
    ///
    /// If the port does not exist in host, it will be inferred from the scheme.
    fn port(&self) -> Option<u16>;
}

impl RequestPartsExt for Parts {
    fn forwarded(&self) -> Forwarded {
        Forwarded::from_header(&self.headers)
    }

    fn request_uri(&self) -> Uri {
        self.uri.clone()
    }

    fn full_uri(&self) -> Option<Uri> {
        let scheme = self.scheme();
        let authority = self.host()?;
        Uri::builder()
            .scheme(scheme)
            .authority(authority)
            .path_and_query(
                self.uri
                    .path_and_query()
                    .map(PathAndQuery::as_str)
                    .unwrap_or("/"),
            )
            .build()
            .ok()
    }

    fn scheme(&self) -> Scheme {
        self.uri.scheme().unwrap_or(&Scheme::HTTP).to_owned()
    }

    fn host(&self) -> Option<&str> {
        match self.headers.get(header::HOST).map(HeaderValue::to_str) {
            Some(Ok(host)) => Some(host),
            _ => None,
        }
    }

    fn port(&self) -> Option<u16> {
        if let Some(port) = self.uri.authority().and_then(Authority::port_u16) {
            return Some(port);
        }
        let scheme = self.scheme();
        if scheme == Scheme::HTTP {
            Some(HTTP_DEFAULT_PORT)
        } else if scheme == Scheme::HTTPS {
            Some(HTTPS_DEFAULT_PORT)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug)]
pub struct Forwarded<'a> {
    pub by: Option<&'a str>,
    pub r#for: Vec<&'a str>,
    pub host: Option<&'a str>,
    pub proto: Option<&'a str>,
}

impl<'a> Forwarded<'a> {
    fn from_header(headers: &'a HeaderMap) -> Self {
        let mut forwarded = Forwarded {
            by: None,
            r#for: Vec::new(),
            host: None,
            proto: None,
        };

        for (name, val) in headers
            .get_all(&header::FORWARDED)
            .into_iter()
            .filter_map(|hdr| hdr.to_str().ok())
            // "for=1.2.3.4, for=5.6.7.8; proto=https"
            .flat_map(|val| val.split(';'))
            // ["for=1.2.3.4, for=5.6.7.8", " proto=https"]
            .flat_map(|vals| vals.split(','))
            // ["for=1.2.3.4", " for=5.6.7.8", " proto=https"]
            .flat_map(|pair| {
                let mut items = pair.trim().splitn(2, '=');
                Some((items.next()?, items.next()?))
            })
        {
            // [(name , val      ), ...                                    ]
            // [("for", "1.2.3.4"), ("for", "5.6.7.8"), ("proto", "https")]

            // taking the first value for each property is correct because spec states that first
            // "for" value is client and rest are proxies; multiple values other properties have
            // no defined semantics
            //
            // > In a chain of proxy servers where this is fully utilized, the first
            // > "for" parameter will disclose the client where the request was first
            // > made, followed by any subsequent proxy identifiers.
            // --- https://datatracker.ietf.org/doc/html/rfc7239#section-5.2

            match name.trim().to_lowercase().as_str() {
                "by" => {
                    if forwarded.by.is_none() {
                        forwarded.by = Some(unquote(val));
                    }
                }
                "for" => {
                    forwarded.r#for.push(unquote(val));
                }
                "host" => {
                    if forwarded.host.is_none() {
                        forwarded.host = Some(unquote(val));
                    }
                }
                "proto" => {
                    if forwarded.proto.is_none() {
                        forwarded.proto = Some(unquote(val));
                    }
                }
                _ => continue,
            };
        }

        forwarded
    }
}

fn unquote(val: &str) -> &str {
    val.trim().trim_start_matches('"').trim_end_matches('"')
}
