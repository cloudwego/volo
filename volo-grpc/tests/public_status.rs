//! C1 uses the old Protobuf entry/stream constructors without a Thrift feature.
use std::time::Duration;

use bytes::Bytes;
use futures::stream;
use http_body::Frame;
use motore::Service;
use volo::{
    context::{Endpoint, Role, RpcInfo},
    net::Address,
};
use volo_grpc::{
    BoxStream, Code, RecvEntryMessage, RecvStream, Request, SendEntryMessage, Status,
    body::BoxBody,
    client::Http2Config,
    codec::{compression::CompressionEncoding, decode::Kind, encode::tests::EchoRequest},
    context::{ClientContext, Config},
    transport::ClientTransport,
};

struct Send;
impl SendEntryMessage for Send {
    fn into_body(
        self,
        compression: Option<CompressionEncoding>,
    ) -> BoxStream<'static, Result<Frame<Bytes>, Status>> {
        volo_grpc::codec::encode::encode(stream::iter([Ok(EchoRequest::default())]), compression)
    }
}
struct Recv(RecvStream<EchoRequest>);
impl RecvEntryMessage for Recv {
    fn from_body(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Ok(Self(RecvStream::<EchoRequest>::new(
            body,
            kind,
            compression,
        )))
    }
}

#[tokio::test]
async fn malformed_details_in_headers_or_final_trailers_do_not_poison_a_protobuf_connection() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cases = [
            (true, "0", "@@@"),
            (true, "13", "@@@"),
            (false, "0", "@@@"),
            (false, "13", "@@@"),
            (true, "13", "AQID/w=="),
            (true, "13", "AQID/w"),
            (false, "0", ""),
        ];
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(socket).await.unwrap();
            let mut requests = Vec::new();
            for (initial, code, details) in cases {
                let (request, mut respond) = connection.accept().await.unwrap().unwrap();
                requests.push(request);
                let mut terminal = http::HeaderMap::new();
                terminal.insert("grpc-status", code.parse().unwrap());
                terminal.insert("x-peer", "kept".parse().unwrap());
                if !details.is_empty() {
                    terminal.insert("grpc-status-details-bin", details.parse().unwrap());
                }
                let mut response = http::Response::builder()
                    .header("content-type", "application/grpc")
                    .body(())
                    .unwrap();
                if initial {
                    response.headers_mut().extend(terminal.clone());
                }
                let mut send = respond.send_response(response, initial).unwrap();
                if !initial {
                    send.send_data(Bytes::from_static(b"\0\0\0\0\0"), false)
                        .unwrap();
                    send.send_trailers(terminal).unwrap();
                }
            }
            drop(requests);
            connection.graceful_shutdown();
            while connection.accept().await.is_some() {}
        });
        let transport = ClientTransport::<Recv>::new(&Http2Config::default(), &Config::default());
        for (index, (initial, raw_code, details)) in cases.into_iter().enumerate() {
            let mut endpoint = Endpoint::new("status-peer".into());
            endpoint.set_address(Address::Ip(address));
            let mut cx = ClientContext::new(RpcInfo::new(
                Role::Client,
                "/public.Status/Call".into(),
                Endpoint::new("probe".into()),
                endpoint,
                Config::default(),
            ));
            let response = transport.call(&mut cx, Request::new(Send)).await;
            let status = if initial {
                response.err().expect("initial error status")
            } else {
                let mut messages = response.unwrap().into_inner().0;
                assert!(messages.message().await.unwrap().is_some());
                if index == cases.len() - 1 {
                    assert!(messages.message().await.unwrap().is_none());
                    continue;
                }
                messages.message().await.unwrap_err()
            };
            assert_eq!(status.code(), Code::Internal);
            assert_eq!(status.raw_grpc_code(), Some(raw_code.as_bytes()));
            assert_eq!(status.metadata().get("x-peer").unwrap(), "kept");
            if details != "@@@" {
                assert_eq!(status.details(), &[1, 2, 3, 255]);
            }
        }
        peer.await.unwrap();
    })
    .await
    .expect("bad details remain per-RPC errors and the same connection remains usable");
}
