use std::{
    io,
    marker::PhantomData,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use bytes::Bytes;
use http::{
    HeaderValue,
    header::{CONTENT_TYPE, TE},
    uri::{Authority, Scheme},
};
use http_body::Frame;
use http_body_util::StreamBody;
use hyper::{
    body::Incoming,
    client::conn::http2::{Builder as Http2Builder, SendRequest},
};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use motore::{Service, make::MakeConnection, service::UnaryService};
use volo::{
    context::Endpoint,
    net::Address,
    pool::{Mode, Pool, Poolable, Pooled, Reservation},
};

use super::connect::Connector;
use crate::{
    Code, Request, Response, Status,
    body::boxed,
    client::Http2Config,
    codec::{
        compression::{ACCEPT_ENCODING_HEADER, ENCODING_HEADER},
        decode::Kind,
    },
    context::{ClientContext, Config},
};

type Body = StreamBody<crate::BoxStream<'static, Result<Frame<Bytes>, Status>>>;

/// Idle connections are dropped after this long; matches what hyper's client pool did.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// A multiplexed HTTP/2 connection to one peer, shared by every call to that peer.
#[derive(Clone)]
struct Http2Connection(SendRequest<Body>);

impl Poolable for Http2Connection {
    async fn reusable(&self) -> bool {
        !self.0.is_closed()
    }

    fn reserve(self) -> Reservation<Self> {
        Reservation::Shared(self.clone(), self)
    }

    fn can_share(&self) -> bool {
        true
    }

    fn try_checkout(&self) -> Option<Self> {
        (!self.0.is_closed()).then(|| self.clone())
    }
}

/// Dials an address and runs the HTTP/2 handshake on it.
#[derive(Clone)]
struct Http2Connector {
    connector: Connector,
    http2_builder: Http2Builder<TokioExecutor>,
}

impl UnaryService<Address> for Http2Connector {
    type Response = Http2Connection;
    type Error = Status;

    async fn call(&self, addr: Address) -> Result<Http2Connection, Status> {
        let io = self
            .connector
            .make_connection(addr)
            .await
            .map_err(|err| Status::from_error(err.into()))?;
        let (mut tx, conn) = self
            .http2_builder
            .handshake::<_, Body>(TokioIo::new(io))
            .await
            .map_err(|err| Status::from_error(err.into()))?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::debug!("[VOLO] http2 client connection error: {err}");
            }
        });
        // Wait for the connection to accept requests before handing it out.
        tx.ready()
            .await
            .map_err(|err| Status::from_error(err.into()))?;
        Ok(Http2Connection(tx))
    }
}

/// gRPC client transport: one HTTP/2 connection per callee address, requests multiplexed on it.
///
/// Connections live in a [`volo::pool::Pool`] keyed by the [`Address`] chosen by service
/// discovery / load balancing, while the request URI is built from the callee [`Endpoint`], so
/// the HTTP/2 `:authority` names the server (the SNI name or the service name) rather than the
/// socket that happens to be dialed. See `authority` in this module for the exact rule.
pub struct ClientTransport<U> {
    connector: Http2Connector,
    pool: Pool<Address, Http2Connection>,
    _marker: PhantomData<fn(U)>,
}

impl<U> Clone for ClientTransport<U> {
    fn clone(&self) -> Self {
        Self {
            connector: self.connector.clone(),
            pool: self.pool.clone(),
            _marker: self._marker,
        }
    }
}

impl<U> ClientTransport<U> {
    /// Creates a new [`ClientTransport`] by setting the underlying connection
    /// with the given config.
    #[must_use]
    pub fn new(http2_config: &Http2Config, rpc_config: &Config) -> Self {
        Self::with_connector(http2_config, Connector::new(Some(dial_config(rpc_config))))
    }

    #[cfg(feature = "__tls")]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "rustls", feature = "native-tls"))))]
    #[must_use]
    pub fn new_with_tls(
        http2_config: &Http2Config,
        rpc_config: &Config,
        tls_config: volo::net::tls::ClientTlsConfig,
    ) -> Self {
        Self::with_connector(
            http2_config,
            Connector::new_with_tls(Some(dial_config(rpc_config)), tls_config),
        )
    }

    fn with_connector(http2_config: &Http2Config, connector: Connector) -> Self {
        Self {
            connector: Http2Connector {
                connector,
                http2_builder: http2_builder(http2_config),
            },
            pool: Pool::new(volo::pool::Config::default().idle_timeout(IDLE_TIMEOUT)),
            _marker: PhantomData,
        }
    }

    async fn connection(&self, addr: &Address) -> Result<Pooled<Address, Http2Connection>, Status> {
        self.pool
            .get(addr.clone(), Mode::Shared, self.connector.clone())
            .await
            .map_err(Status::from)
    }

    async fn send(
        &self,
        addr: &Address,
        req: http::Request<Body>,
    ) -> Result<http::Response<Incoming>, Status> {
        let mut conn = self.connection(addr).await?;
        let mut err = match conn.0.try_send_request(req).await {
            Ok(resp) => return Ok(resp),
            Err(err) => err,
        };
        // The connection went away between the pool's liveness check and the dispatch. hyper
        // hands the request back untouched in that case, so retry it once on a fresh connection;
        // the pool sees the closed one and replaces it.
        if let Some(req) = err.take_message() {
            tracing::debug!("[VOLO] http2 connection to {addr} was closed, reconnecting");
            let mut conn = self.connection(addr).await?;
            return conn
                .0
                .send_request(req)
                .await
                .map_err(|err| Status::from_error(err.into()));
        }
        Err(Status::from_error(err.into_error().into()))
    }
}

fn dial_config(rpc_config: &Config) -> volo::net::dial::Config {
    volo::net::dial::Config::new(
        rpc_config.connect_timeout,
        rpc_config.read_timeout,
        rpc_config.write_timeout,
    )
}

fn http2_builder(config: &Http2Config) -> Http2Builder<TokioExecutor> {
    let mut builder = Http2Builder::new(TokioExecutor::new());
    builder
        .timer(TokioTimer::new())
        .initial_stream_window_size(config.init_stream_window_size)
        .initial_connection_window_size(config.init_connection_window_size)
        .max_frame_size(config.max_frame_size)
        .adaptive_window(config.adaptive_window)
        .keep_alive_interval(config.http2_keepalive_interval)
        .keep_alive_timeout(config.http2_keepalive_timeout)
        .keep_alive_while_idle(config.http2_keepalive_while_idle)
        .max_concurrent_reset_streams(config.max_concurrent_reset_streams)
        .max_send_buf_size(config.max_send_buf_size);
    builder
}

impl<T, U> Service<ClientContext, Request<T>> for ClientTransport<U>
where
    T: crate::message::SendEntryMessage + Send + 'static,
    U: crate::message::RecvEntryMessage + 'static,
{
    type Response = Response<U>;

    type Error = Status;

    #[cfg_attr(not(feature = "compress"), allow(unused_variables))]
    async fn call(
        &self,
        cx: &mut ClientContext,
        volo_req: Request<T>,
    ) -> Result<Self::Response, Self::Error> {
        // SAFETY: parameters controlled by volo-grpc are guaranteed to be valid.
        // get the call address from the context
        let target = cx.rpc_info.callee().address().ok_or_else(|| {
            io::Error::new(std::io::ErrorKind::InvalidData, "address is required")
        })?;

        let (metadata, extensions, message) = volo_req.into_parts();
        let path = cx.rpc_info.method();
        let rpc_config = cx.rpc_info.config();
        let accept_compressions = &rpc_config.accept_compressions;

        // select the compression algorithm with the highest priority by user's config
        let send_compression = rpc_config
            .send_compressions
            .as_ref()
            .map(|config| config[0]);

        let body = http_body_util::StreamBody::new(message.into_body(send_compression));

        let uri = build_uri(
            self.connector.connector.scheme(),
            authority(
                self.connector.connector.tls_server_name(),
                cx.rpc_info.callee(),
                &target,
            ),
            path,
        );
        let mut req = http::Request::builder()
            .version(http::Version::HTTP_2)
            .method(http::Method::POST)
            .uri(uri)
            .extension(extensions)
            .body(body)
            .map_err(|err| Status::from_error(err.into()))?;
        *req.headers_mut() = metadata.into_headers();
        req.headers_mut()
            .insert(TE, HeaderValue::from_static("trailers"));
        req.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));

        // insert compression headers
        if let Some(send_compression) = send_compression {
            req.headers_mut()
                .insert(ENCODING_HEADER, send_compression.into_header_value());
        }
        if let Some(accept_compressions) = accept_compressions {
            if !accept_compressions.is_empty() {
                if let Some(header_value) =
                    accept_compressions[0].into_accept_encoding_header_value(accept_compressions)
                {
                    req.headers_mut()
                        .insert(ACCEPT_ENCODING_HEADER, header_value);
                }
            }
        }
        cx.stats.record_make_transport_start_at();

        let resp = self.send(&target, req).await?;

        cx.stats.record_make_transport_end_at();

        let status_code = resp.status();
        let headers = resp.headers();

        if let Some(status) = Status::from_header_map(headers) {
            if status.code() != Code::Ok {
                return Err(status);
            }
        }
        let path = cx.rpc_info.method();
        let rpc_config = cx.rpc_info.config();

        #[cfg(not(feature = "compress"))]
        let accept_compression = None;
        #[cfg(feature = "compress")]
        let accept_compression =
            crate::codec::compression::CompressionEncoding::from_encoding_header(
                headers,
                &rpc_config.accept_compressions,
            )?;

        let (parts, body) = resp.into_parts();

        let body = U::from_body(
            Some(path),
            boxed(body),
            Kind::Response(status_code),
            accept_compression,
        )?;
        let resp = hyper::Response::from_parts(parts, body);
        Ok(Response::from_http(resp))
    }
}

/// Picks the HTTP/2 `:authority` for a call.
///
/// gRPC uses `:authority` as the virtual host of the callee, so it has to name the server rather
/// than the socket that happens to be dialed: TLS-terminating proxies and ingresses route on it,
/// and it has to agree with the TLS SNI. In order of preference:
///
/// 0. an explicit override on the callee endpoint, tagged [`crate::client::Authority`];
/// 1. the TLS server name (the SNI), with the dialed port appended unless it is the default 443;
/// 2. the callee's service name, which is the `host[:port]` given to the DNS resolver, or the
///    logical name used with a custom `Discover`;
/// 3. the dialed address itself.
// Which `Address` variants exist depends on the target and on volo's features, so the
// non-IP arms have to be wildcards.
#[allow(clippy::match_wildcard_for_single_variants)]
fn authority(tls_server_name: Option<&str>, callee: &Endpoint, addr: &Address) -> Authority {
    if let Some(authority) = callee
        .get_faststr::<crate::client::Authority>()
        .and_then(|name| parse_authority(name))
    {
        return authority;
    }

    let port = match addr {
        Address::Ip(addr) => Some(addr.port()),
        #[allow(unreachable_patterns)]
        _ => None,
    };

    if let Some(name) = tls_server_name {
        let host_port = match (name.parse::<IpAddr>(), port) {
            (Ok(ip), Some(port)) => SocketAddr::new(ip, port).to_string(),
            (Ok(IpAddr::V6(ip)), None) => format!("[{ip}]"),
            (Err(_), Some(port)) if port != 443 => format!("{name}:{port}"),
            _ => name.to_owned(),
        };
        if let Ok(authority) = Authority::from_str(&host_port) {
            return authority;
        }
    }

    if let Some(authority) = parse_authority(callee.service_name_ref()) {
        return authority;
    }

    match addr {
        Address::Ip(addr) => {
            Authority::from_str(&addr.to_string()).expect("socket addr is a valid authority")
        }
        #[allow(unreachable_patterns)]
        _ => Authority::from_static("localhost"),
    }
}

/// Parses a user-supplied name as an `:authority`, rejecting what HTTP/2 forbids there: an
/// empty value, or one carrying userinfo.
fn parse_authority(name: &str) -> Option<Authority> {
    if name.is_empty() || name.contains('@') {
        return None;
    }
    Authority::from_str(name).ok()
}

fn build_uri(scheme: Scheme, authority: Authority, path: &str) -> hyper::Uri {
    hyper::Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query(path)
        .build()
        .expect("fail to build uri")
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use http::uri::{Authority, Scheme};
    use volo::{FastStr, context::Endpoint, net::Address};

    use super::{authority, build_uri};

    fn endpoint(service_name: &str) -> Endpoint {
        Endpoint::new(FastStr::new(service_name))
    }

    fn ip(addr: &str) -> Address {
        Address::from(addr.parse::<SocketAddr>().unwrap())
    }

    #[test]
    fn authority_tag_overrides_everything() {
        let mut callee = endpoint("grpc.example.com:50051");
        callee.insert_faststr::<crate::client::Authority>(FastStr::from_static_str(
            "users.mesh.local:50051",
        ));
        let a = authority(Some("grpc.example.com"), &callee, &ip("10.0.0.1:50051"));
        assert_eq!(a, Authority::from_static("users.mesh.local:50051"));
    }

    #[test]
    fn authority_tag_that_is_not_an_authority_is_ignored() {
        for bad in ["", "user@host", "http://host", "not a host"] {
            let mut callee = endpoint("grpc.example.com:50051");
            callee.insert_faststr::<crate::client::Authority>(FastStr::new(bad));
            let a = authority(None, &callee, &ip("10.0.0.1:50051"));
            assert_eq!(
                a,
                Authority::from_static("grpc.example.com:50051"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn authority_prefers_tls_server_name_with_dialed_port() {
        let a = authority(
            Some("grpc.example.com"),
            &endpoint("some-logical-name"),
            &ip("10.0.0.1:50051"),
        );
        assert_eq!(a, Authority::from_static("grpc.example.com:50051"));
    }

    #[test]
    fn authority_omits_default_https_port_for_tls_server_name() {
        let a = authority(
            Some("grpc.example.com"),
            &endpoint("grpc.example.com:443"),
            &ip("10.0.0.1:443"),
        );
        assert_eq!(a, Authority::from_static("grpc.example.com"));
    }

    #[test]
    fn authority_handles_ip_server_names() {
        let a = authority(Some("::1"), &endpoint("svc"), &ip("[::1]:8080"));
        assert_eq!(a, Authority::from_static("[::1]:8080"));
        let a = authority(Some("127.0.0.1"), &endpoint("svc"), &ip("127.0.0.1:8080"));
        assert_eq!(a, Authority::from_static("127.0.0.1:8080"));
    }

    #[test]
    fn authority_falls_back_to_service_name() {
        let a = authority(
            None,
            &endpoint("grpc.example.com:50051"),
            &ip("10.0.0.1:50051"),
        );
        assert_eq!(a, Authority::from_static("grpc.example.com:50051"));
        let a = authority(None, &endpoint("user-service"), &ip("10.0.0.1:50051"));
        assert_eq!(a, Authority::from_static("user-service"));
    }

    #[test]
    fn authority_falls_back_to_address_for_unusable_service_names() {
        for name in ["", "not a host", "user@host", "http://host:80"] {
            let a = authority(None, &endpoint(name), &ip("10.0.0.1:50051"));
            assert_eq!(a, Authority::from_static("10.0.0.1:50051"), "{name:?}");
        }
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn authority_for_unix_socket_without_service_name() {
        let addr = std::os::unix::net::SocketAddr::from_pathname("/tmp/rpc.sock").unwrap();
        let a = authority(None, &endpoint(""), &Address::from(addr));
        assert_eq!(a, Authority::from_static("localhost"));
    }

    #[test]
    fn test_build_uri() {
        let uri = build_uri(
            Scheme::HTTP,
            Authority::from_static("127.0.0.1:8000"),
            "/path?query=1",
        );
        assert_eq!(
            uri,
            "http://127.0.0.1:8000/path?query=1"
                .parse::<hyper::Uri>()
                .unwrap()
        );
        let uri = build_uri(
            Scheme::HTTPS,
            Authority::from_static("grpc.example.com:50051"),
            "/pkg.Svc/Method",
        );
        assert_eq!(
            uri,
            "https://grpc.example.com:50051/pkg.Svc/Method"
                .parse::<hyper::Uri>()
                .unwrap()
        );
    }

    fn is_unpin<T: Unpin>() {}

    #[test]
    fn test_is_unpin() {
        is_unpin::<super::ClientTransport<()>>();
    }

    mod wire {
        //! Drive the transport against a real HTTP/2 server and look at what arrives.

        use std::{
            net::SocketAddr,
            sync::{
                Arc, Mutex,
                atomic::{AtomicUsize, Ordering},
            },
        };

        use bytes::Bytes;
        use futures::StreamExt;
        use http::header::CONTENT_TYPE;
        use http_body::Frame;
        use http_body_util::Empty;
        use hyper::service::service_fn;
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use motore::Service;
        use tokio::net::TcpListener;
        use volo::{
            FastStr,
            context::{Endpoint, Role, RpcInfo},
            net::Address,
        };

        use crate::{
            Code, Request, Status,
            body::BoxBody,
            client::Http2Config,
            codec::{compression::CompressionEncoding, decode::Kind},
            context::{ClientContext, Config},
            message::{RecvEntryMessage, SendEntryMessage},
            transport::ClientTransport,
        };

        struct Empty_;

        impl SendEntryMessage for Empty_ {
            fn into_body(
                self,
                _: Option<CompressionEncoding>,
            ) -> crate::BoxStream<'static, Result<Frame<Bytes>, Status>> {
                futures::stream::empty().boxed()
            }
        }

        impl RecvEntryMessage for Empty_ {
            fn from_body(
                _: Option<&str>,
                _: BoxBody,
                _: Kind,
                _: Option<CompressionEncoding>,
            ) -> Result<Self, Status> {
                Ok(Self)
            }
        }

        #[derive(Default)]
        struct Observed {
            authorities: Mutex<Vec<String>>,
            connections: AtomicUsize,
        }

        /// A plain HTTP/2 server that records `:authority` of every request and answers each
        /// with `grpc-status: UNIMPLEMENTED` in the headers, which the client surfaces as an
        /// error without ever touching the body.
        async fn serve() -> (SocketAddr, Arc<Observed>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let observed = Arc::new(Observed::default());
            let observed_ = observed.clone();
            tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    observed_.connections.fetch_add(1, Ordering::SeqCst);
                    let observed = observed_.clone();
                    tokio::spawn(async move {
                        let svc = service_fn(move |req: http::Request<hyper::body::Incoming>| {
                            let observed = observed.clone();
                            async move {
                                let authority = req
                                    .uri()
                                    .authority()
                                    .map(|a| a.to_string())
                                    .unwrap_or_default();
                                observed.authorities.lock().unwrap().push(authority);
                                http::Response::builder()
                                    .header(CONTENT_TYPE, "application/grpc")
                                    .header("grpc-status", Code::Unimplemented as i32)
                                    .body(Empty::<Bytes>::new())
                            }
                        });
                        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                            .serve_connection(TokioIo::new(stream), svc)
                            .await;
                    });
                }
            });
            (addr, observed)
        }

        fn cx(service_name: &str, addr: SocketAddr) -> ClientContext {
            let mut callee = Endpoint::new(FastStr::new(service_name));
            callee.set_address(Address::from(addr));
            ClientContext::new(RpcInfo::new(
                Role::Client,
                FastStr::from_static_str("/pkg.Svc/Method"),
                Endpoint::new(FastStr::from_static_str("caller")),
                callee,
                Config::default(),
            ))
        }

        async fn call(transport: &ClientTransport<Empty_>, cx: &mut ClientContext) {
            let err = transport
                .call(cx, Request::new(Empty_))
                .await
                .err()
                .expect("server answers with a non-OK grpc-status");
            assert_eq!(err.code(), Code::Unimplemented, "{err:?}");
        }

        #[tokio::test]
        async fn authority_is_the_service_name_and_connections_are_reused() {
            let (addr, observed) = serve().await;
            let transport =
                ClientTransport::<Empty_>::new(&Http2Config::default(), &Config::default());

            call(&transport, &mut cx("grpc.example.com:50051", addr)).await;
            call(&transport, &mut cx("grpc.example.com:50051", addr)).await;
            // A different logical callee behind the same address still shares the connection.
            call(&transport, &mut cx("user-service", addr)).await;

            assert_eq!(
                *observed.authorities.lock().unwrap(),
                [
                    "grpc.example.com:50051",
                    "grpc.example.com:50051",
                    "user-service"
                ]
            );
            assert_eq!(observed.connections.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn authority_tag_is_sent_on_the_wire() {
            let (addr, observed) = serve().await;
            let transport =
                ClientTransport::<Empty_>::new(&Http2Config::default(), &Config::default());

            let mut cx = cx("grpc.example.com:50051", addr);
            cx.rpc_info
                .callee_mut()
                .insert_faststr::<crate::client::Authority>(FastStr::from_static_str(
                    "users.mesh.local:50051",
                ));
            call(&transport, &mut cx).await;

            assert_eq!(
                *observed.authorities.lock().unwrap(),
                ["users.mesh.local:50051"]
            );
        }

        #[tokio::test]
        async fn authority_falls_back_to_the_address() {
            let (addr, observed) = serve().await;
            let transport =
                ClientTransport::<Empty_>::new(&Http2Config::default(), &Config::default());

            call(&transport, &mut cx("", addr)).await;

            assert_eq!(*observed.authorities.lock().unwrap(), [addr.to_string()]);
        }

        #[tokio::test]
        async fn concurrent_first_calls_share_one_handshake() {
            let (addr, observed) = serve().await;
            let transport =
                ClientTransport::<Empty_>::new(&Http2Config::default(), &Config::default());

            futures::future::join_all((0..32).map(|_| {
                let transport = transport.clone();
                async move { call(&transport, &mut cx("user-service", addr)).await }
            }))
            .await;

            assert_eq!(observed.authorities.lock().unwrap().len(), 32);
            assert_eq!(observed.connections.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn distinct_addresses_get_distinct_connections() {
            let (addr_a, observed_a) = serve().await;
            let (addr_b, observed_b) = serve().await;
            let transport =
                ClientTransport::<Empty_>::new(&Http2Config::default(), &Config::default());

            call(&transport, &mut cx("user-service", addr_a)).await;
            call(&transport, &mut cx("user-service", addr_b)).await;
            call(&transport, &mut cx("user-service", addr_a)).await;

            assert_eq!(observed_a.connections.load(Ordering::SeqCst), 1);
            assert_eq!(observed_b.connections.load(Ordering::SeqCst), 1);
            assert_eq!(observed_a.authorities.lock().unwrap().len(), 2);
            assert_eq!(observed_b.authorities.lock().unwrap().len(), 1);
        }
    }
}
