#![cfg(feature = "thrift")]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures::{StreamExt, stream};
use http::{HeaderMap, HeaderValue, StatusCode};
use motore::Service;
use pilota::thrift::{ApplicationException, ApplicationExceptionKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use volo::net::incoming::DefaultIncoming;
use volo_grpc::{
    BoxStream, RecvEntryMessage, Request, Response, SendEntryMessage, Status,
    body::{Body, BoxBody, boxed},
    client::Http2Config,
    codec::{
        CodecLimits,
        compression::CompressionEncoding,
        decode::Kind,
        encode::encode_with_options,
        thrift::{ThriftDecoder, ThriftEncoder, ThriftRecvStream},
    },
    context::{ClientContext, Config, ServerContext},
    message::{
        CodecOptions, MethodDescriptor, PayloadKind, PayloadShape, RpcMode, ServiceDescriptor,
    },
    server::{NamedService, Server, ServiceBuilder},
    transport::ClientTransport,
};

static DECODER_CALLS: AtomicUsize = AtomicUsize::new(0);

struct WireRequest(ThriftRecvStream<ApplicationException>);
impl RecvEntryMessage for WireRequest {
    fn payload_kind() -> PayloadKind {
        PayloadKind::Thrift
    }
    fn from_body(
        method: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Self::from_body_with_options(
            method,
            body,
            kind,
            compression,
            CodecOptions::new(CodecLimits::default()),
        )
    }
    fn from_body_with_options(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
    ) -> Result<Self, Status> {
        DECODER_CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(Self(
            ThriftRecvStream::with_decoder(body, kind, compression, ThriftDecoder::default())
                .with_limits(options.limits),
        ))
    }
}

struct WireReply(BoxStream<'static, Result<ApplicationException, Status>>);
impl SendEntryMessage for WireReply {
    fn payload_kind(&self) -> PayloadKind {
        PayloadKind::Thrift
    }
    fn into_body(
        self,
        compression: Option<CompressionEncoding>,
    ) -> BoxStream<'static, Result<http_body::Frame<Bytes>, Status>> {
        self.into_body_with_options(compression, CodecOptions::new(CodecLimits::default()))
    }
    fn into_body_with_options(
        self,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
    ) -> BoxStream<'static, Result<http_body::Frame<Bytes>, Status>> {
        encode_with_options(
            self.0,
            compression,
            ThriftEncoder::default(),
            options.limits,
        )
    }
}

#[derive(Clone)]
struct ThriftService(Arc<AtomicUsize>);
impl NamedService for ThriftService {
    const NAME: &'static str = "wire.Thrift";
    fn descriptor() -> Option<ServiceDescriptor> {
        Some(ServiceDescriptor {
            payload: PayloadKind::Thrift,
            methods: &[
                MethodDescriptor {
                    name: "Echo",
                    mode: RpcMode::Bidirectional,
                    payload: PayloadShape::StreamingRawStruct,
                },
                MethodDescriptor {
                    name: "Error",
                    mode: RpcMode::Bidirectional,
                    payload: PayloadShape::StreamingRawStruct,
                },
            ],
        })
    }
}
impl Service<ServerContext, Request<WireRequest>> for ThriftService {
    type Response = Response<WireReply>;
    type Error = Status;
    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<WireRequest>,
    ) -> Result<Self::Response, Status> {
        self.0.fetch_add(1, Ordering::Relaxed);
        if cx.rpc_info.method().ends_with("/Error") {
            return Err(Status::aborted("handler error"));
        }
        let mut input = req.into_inner().0;
        let value = input
            .next()
            .await
            .ok_or_else(|| Status::internal("missing request"))??;
        Ok(Response::new(WireReply(Box::pin(stream::iter([Ok(
            value,
        )])))))
    }
}

#[derive(Clone)]
struct ProtoService;
impl NamedService for ProtoService {
    const NAME: &'static str = "wire.Proto";
}
impl Service<ServerContext, Request<BoxBody>> for ProtoService {
    type Response = Response<BoxBody>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        _: Request<BoxBody>,
    ) -> Result<Self::Response, Status> {
        Ok(Response::new(boxed(Body::new(Box::pin(stream::empty())))))
    }
}

struct Observed {
    http: StatusCode,
    headers: HeaderMap,
    data: Vec<u8>,
    trailers: Option<HeaderMap>,
}
impl Observed {
    fn code(&self) -> &HeaderValue {
        self.headers
            .get("grpc-status")
            .or_else(|| self.trailers.as_ref().and_then(|t| t.get("grpc-status")))
            .unwrap()
    }
}

async fn request(
    client: &mut h2::client::SendRequest<Bytes>,
    address: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, HeaderValue)],
) -> Observed {
    let mut req = http::Request::builder()
        .method("POST")
        .uri(format!("http://{address}{path}"))
        .body(())
        .unwrap();
    for (key, value) in headers {
        req.headers_mut().append(
            http::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value.clone(),
        );
    }
    let (response, mut send) = client.send_request(req, false).unwrap();
    send.send_data(Bytes::from_static(b"\0\0\0\0\x01\0"), true)
        .unwrap();
    let response = response.await.unwrap();
    let http = response.status();
    let headers = response.headers().clone();
    let mut body = response.into_body();
    let mut data = vec![];
    while let Some(bytes) = body.data().await {
        let bytes = bytes.unwrap();
        data.extend_from_slice(&bytes);
        body.flow_control().release_capacity(bytes.len()).unwrap();
    }
    let trailers = body.trailers().await.unwrap();
    Observed {
        http,
        headers,
        data,
        trailers,
    }
}

#[tokio::test]
async fn content_type_routes_before_decoding_and_preserves_error_response_formats() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handlers = Arc::new(AtomicUsize::new(0));
        let thrift = ServiceBuilder::new(ThriftService(handlers.clone())).build::<WireRequest, WireReply>();
        let server = Server::new().accept_http1(true).add_service(thrift).add_service(ProtoService);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            server.run_with_shutdown(DefaultIncoming::from(listener), async { stop_rx.await.unwrap(); Ok(()) }).await.unwrap();
        });
        let socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, connection) = h2::client::handshake(socket).await.unwrap();
        let connection_task = tokio::spawn(connection);
        for subtype in ["application/grpc", "application/grpc+proto", "application/grpc+json"] {
            let observed = request(&mut client, address, "/wire.Thrift/Echo", &[("content-type", subtype.parse().unwrap())]).await;
            assert_eq!(observed.http, StatusCode::OK);
            assert_eq!(observed.code(), "12");
            assert_eq!(observed.headers["content-type"], "application/grpc+thrift");
            assert!(observed.data.is_empty());
        }
        for headers in [vec![], vec![("content-type", HeaderValue::from_static("text/plain"))],
                        vec![("content-type", HeaderValue::from_static("application/grpc+thrift")), ("content-type", HeaderValue::from_static("application/grpc+proto"))],
                        vec![("content-type", HeaderValue::from_bytes(b"\xff").unwrap())]] {
            let observed = request(&mut client, address, "/wire.Thrift/Echo", &headers).await;
            assert_eq!(observed.http, StatusCode::UNSUPPORTED_MEDIA_TYPE);
            assert_eq!(observed.headers["content-type"], "application/grpc+thrift");
            assert!(observed.data.is_empty());
        }
        let unknown = request(&mut client, address, "/wire.Thrift/Missing", &[("content-type", HeaderValue::from_static("text/plain"))]).await;
        assert_eq!(unknown.code(), "12");
        assert_eq!(unknown.http, StatusCode::OK);
        assert_eq!(unknown.headers["content-type"], "application/grpc+thrift");
        let expired = request(&mut client, address, "/wire.Thrift/Echo", &[
            ("content-type", HeaderValue::from_static("application/grpc+thrift")), ("grpc-timeout", HeaderValue::from_static("0n"))]).await;
        assert_eq!(expired.code(), "4");
        assert_eq!(expired.headers["content-type"], "application/grpc+thrift");
        for encoding in ["unknown", "gzip"] {
            let unsupported = request(&mut client, address, "/wire.Thrift/Echo", &[
                ("content-type", HeaderValue::from_static("application/grpc+thrift")), ("grpc-encoding", encoding.parse().unwrap())]).await;
            assert_eq!(unsupported.code(), "12");
            assert_eq!(unsupported.headers["content-type"], "application/grpc+thrift");
        }
        assert_eq!(handlers.load(Ordering::Relaxed), 0);
        assert_eq!(DECODER_CALLS.load(Ordering::Relaxed), 0);

        // Explicit HTTP/1 support is a separate existing server configuration.
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket.write_all(b"POST /wire.Thrift/Echo HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut http1 = String::new();
        socket.read_to_string(&mut http1).await.unwrap();
        assert!(http1.starts_with("HTTP/1.1 415"), "{http1}");
        assert_eq!(handlers.load(Ordering::Relaxed), 0);
        assert_eq!(DECODER_CALLS.load(Ordering::Relaxed), 0);

        let ok = request(&mut client, address, "/wire.Thrift/Echo", &[
            ("content-type", HeaderValue::from_static("application/grpc+thrift; charset=utf-8")),
            ("x-opaque", HeaderValue::from_bytes("中文".as_bytes()).unwrap()), ("trace-bin", HeaderValue::from_static("AQID"))]).await;
        assert_eq!(ok.code(), "0");
        assert_eq!(ok.headers["content-type"], "application/grpc+thrift");
        assert!(!ok.data.is_empty());
        let error = request(&mut client, address, "/wire.Thrift/Error", &[("content-type", HeaderValue::from_static("application/grpc+thrift"))]).await;
        assert_eq!(error.code(), "10");
        assert_eq!(error.headers["content-type"], "application/grpc+thrift");
        let proto = request(&mut client, address, "/wire.Proto/Call", &[("content-type", HeaderValue::from_static("text/plain"))]).await;
        assert_eq!(proto.code(), "0");
        assert_eq!(proto.headers["content-type"], "application/grpc");

        // The actual Rust client transport chooses Thrift headers and codec
        // options; the service coexists with the Protobuf route on this socket.
        let transport = ClientTransport::<WireRequest>::new(&Http2Config::default(), &Config::default());
        let mut cx = ClientContext::default();
        cx.rpc_info.callee_mut().set_address(volo::net::Address::from(address));
        cx.rpc_info.set_method("/wire.Thrift/Echo".into());
        let value = ApplicationException::new(ApplicationExceptionKind::UNKNOWN_METHOD, "echo");
        let input = WireReply(Box::pin(stream::iter([Ok(value)])));
        let mut response = transport.call(&mut cx, Request::new(input)).await.unwrap().into_inner().0;
        assert_eq!(response.next().await.unwrap().unwrap().message(), "echo");
        assert!(response.next().await.is_none());
        drop(transport);
        drop(client);
        connection_task.abort();
        let _ = connection_task.await;
        stop_tx.send(()).unwrap();
        server_task.await.unwrap();
    }).await.expect("content-type matrix must finish within its budget");
}
