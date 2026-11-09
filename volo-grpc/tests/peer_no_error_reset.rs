//! A peer can finish its response, then stop the still-open upload with NO_ERROR.
use std::time::Duration;

use bytes::Bytes;
use futures::stream;
use http_body::Frame;
use motore::Service;
use volo::context::{Endpoint, Role, RpcInfo};
use volo_grpc::{
    RecvEntryMessage, Request, SendEntryMessage, Status,
    body::BoxBody,
    client::Http2Config,
    codec::{compression::CompressionEncoding, decode::Kind},
    context::{ClientContext, Config},
    streaming::StreamContext,
    transport::ClientTransport,
};

struct PendingUpload;
impl SendEntryMessage for PendingUpload {
    fn into_body(
        self,
        _: Option<CompressionEncoding>,
    ) -> volo_grpc::BoxStream<'static, Result<Frame<Bytes>, Status>> {
        Box::pin(stream::pending())
    }
}
struct Reply(volo_grpc::RecvStream<Bytes>);
impl RecvEntryMessage for Reply {
    fn from_body(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Ok(Self(volo_grpc::RecvStream::new(body, kind, compression)))
    }
}

#[tokio::test]
async fn trailer_status_survives_no_error_but_missing_status_and_cancel_still_fail() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for (terminal, reset, expected) in [
            (Some("0"), h2::Reason::NO_ERROR, volo_grpc::Code::Ok),
            (Some("4"), h2::Reason::NO_ERROR, volo_grpc::Code::DeadlineExceeded),
            (None, h2::Reason::NO_ERROR, volo_grpc::Code::Internal),
            (None, h2::Reason::CANCEL, volo_grpc::Code::Cancelled),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let peer = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(socket).await.unwrap();
                while let Some(request) = connection.accept().await {
                    let (_, mut response) = request.unwrap();
                    let mut send = response.send_response(http::Response::builder().header("content-type", "application/grpc").body(()).unwrap(), false).unwrap();
                    send.send_data(Bytes::from_static(b"\0\0\0\0\0"), false).unwrap();
                    if let Some(code) = terminal {
                        let mut trailer = http::HeaderMap::new(); trailer.insert("grpc-status", code.parse().unwrap());
                        send.send_trailers(trailer).unwrap();
                    }
                    tokio::select! { biased;
                        next = connection.accept() => panic!("unexpected request before reset: {}", next.is_some()),
                        _ = std::future::ready(()) => {},
                    }
                    send.send_reset(reset);
                }
            });
            let transport = ClientTransport::<Reply>::new(&Http2Config::default(), &Config::default());
            for _ in 0..16 {
                let mut callee = Endpoint::new("peer".into()); callee.set_address(address.into());
                let mut cx = ClientContext::new(RpcInfo::new(Role::Client, "/test/Call".into(), Endpoint::new("caller".into()), callee, Config::default()));
                let mut request = Request::new(PendingUpload);
                request.extensions_mut().insert(StreamContext::new("/test/Call", None, Role::Client, None));
                let status = match transport.call(&mut cx, request).await {
                    Err(error) => error.code(),
                    Ok(response) => {
                        let mut body = response.into_inner().0;
                        loop { match body.message().await {
                            Ok(Some(_)) => {}, Ok(None) => break volo_grpc::Code::Ok,
                            Err(error) => break error.code(),
                        }}
                    },
                };
                assert_eq!(status, expected, "terminal={terminal:?}, reset={reset:?}");
            }
            drop(transport); peer.abort(); let _ = peer.await;
        }
    }).await.expect("terminal validation must stay bounded");
}

#[tokio::test]
async fn final_status_wins_over_no_error_reset_of_the_open_upload() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for code in ["4", "0"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut connection = h2::server::handshake(socket).await.unwrap();
            while let Some(request) = connection.accept().await {
                let (_, mut response) = request.unwrap();
                let mut send = response
                    .send_response(
                        http::Response::builder()
                            .header("content-type", "application/grpc")
                            .header("grpc-status", code)
                            .body(())
                            .unwrap(),
                        true,
                    )
                    .unwrap();
                // Drive the connection once so HEADERS reach the socket before
                // queuing RST. Resetting an unflushed h2 response can discard
                // it, which would not be a valid terminal-status reproduction.
                tokio::select! { biased;
                    next = connection.accept() => panic!("unexpected request before reset: {}", next.is_some()),
                    _ = std::future::ready(()) => {},
                }
                send.send_reset(h2::Reason::NO_ERROR);
            }
        });
        let transport = ClientTransport::<Reply>::new(&Http2Config::default(), &Config::default());
        for _ in 0..64 {
            let mut callee = Endpoint::new("peer".into());
            callee.set_address(address.into());
            let mut cx = ClientContext::new(RpcInfo::new(
                Role::Client,
                "/test/Call".into(),
                Endpoint::new("caller".into()),
                callee,
                Config::default(),
            ));
            let mut request = Request::new(PendingUpload);
            request.extensions_mut().insert(StreamContext::new(
                "/test/Call",
                None,
                Role::Client,
                None,
            ));
            let response = transport.call(&mut cx, request).await;
            if code == "4" {
                let status = response.err().expect("peer status must be returned");
                assert_eq!(status.code(), volo_grpc::Code::DeadlineExceeded, "{status:?}");
            } else {
                let mut response = response.unwrap().into_inner().0;
                assert!(response.message().await.unwrap().is_none());
            }
        }
        drop(transport);
        peer.abort();
        let _ = peer.await;
        }
    })
    .await
    .expect("reset race must remain bounded");
}

#[tokio::test]
async fn queued_response_trailer_survives_upload_reset_before_the_consumer_resumes() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for code in ["0", "4"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let peer = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut connection = h2::server::handshake(socket).await.unwrap();
                let mut uploads = Vec::new();
                while let Some(request) = connection.accept().await {
                    let (request, mut response) = request.unwrap();
                    uploads.push(request.into_body());
                    let mut send = response.send_response(
                        http::Response::builder().header("content-type", "application/grpc").body(()).unwrap(), false,
                    ).unwrap();
                    // More frames than the client's bounded handoff queue. Its
                    // reader must wait for the application while reset wakes
                    // the still-pending upload independently.
                    for _ in 0..4 {
                        send.send_data(Bytes::from_static(b"\0\0\0\0\0"), false).unwrap();
                    }
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("grpc-status", code.parse().unwrap());
                    send.send_trailers(trailers).unwrap();
                    tokio::select! { biased;
                        next = connection.accept() => panic!("unexpected request before reset: {}", next.is_some()),
                        _ = std::future::ready(()) => {},
                    }
                    send.send_reset(h2::Reason::NO_ERROR);
                }
            }));
            let transport = ClientTransport::<Reply>::new(&Http2Config::default(), &Config::default());
            for _ in 0..32 {
                let mut callee = Endpoint::new("peer".into());
                callee.set_address(address.into());
                let mut cx = ClientContext::new(RpcInfo::new(Role::Client, "/test/Call".into(), Endpoint::new("caller".into()), callee, Config::default()));
                let mut request = Request::new(PendingUpload);
                request.extensions_mut().insert(StreamContext::new("/test/Call", None, Role::Client, None));
                let mut response = transport.call(&mut cx, request).await.unwrap().into_inner().0;
                tokio::time::sleep(Duration::from_millis(5)).await;
                for _ in 0..4 {
                    assert!(response.message().await.expect("queued messages survive upload closure").is_some());
                }
                let end = response.message().await;
                if code == "0" {
                    assert!(end.unwrap().is_none());
                } else {
                    assert_eq!(end.unwrap_err().code(), volo_grpc::Code::DeadlineExceeded);
                }
            }
            drop((transport, peer));
        }
    }).await.expect("queued terminal handling must be bounded");
}
