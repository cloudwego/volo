//! Exercise both metainfo task handoffs through actual H2 HEADERS and Trailers.
use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use metainfo::{Backward, Forward, METAINFO};
use motore::Service;
use tokio::sync::Notify;
use volo::net::conn::PeekedConn;
use volo_grpc::{
    RecvEntryMessage, RecvStream, Request, Response, Status,
    body::BoxBody,
    codec::{compression::CompressionEncoding, decode::Kind, encode::tests::EchoRequest},
    context::ServerContext,
    message::{MethodDescriptor, PayloadKind, PayloadShape, RpcMode, ServiceDescriptor},
    server::{MetaService, NamedService, Router, ServiceBuilder, serve_detected},
    streaming::{
        StreamContext,
        response::{self, ProtobufStream},
    },
    tracing::DefaultProvider,
};

struct Input(RecvStream<EchoRequest>);
impl RecvEntryMessage for Input {
    fn from_body(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Ok(Self(RecvStream::new(body, kind, compression)))
    }
}

#[derive(Clone)]
struct Producer(Arc<Notify>);
impl NamedService for Producer {
    const NAME: &'static str = "test.Producer";
    fn descriptor() -> Option<ServiceDescriptor> {
        Some(ServiceDescriptor {
            payload: PayloadKind::Protobuf,
            methods: &[
                MethodDescriptor {
                    name: "Watch",
                    mode: RpcMode::ServerStreaming,
                    payload: PayloadShape::ProtobufMessage,
                },
                MethodDescriptor {
                    name: "Chat",
                    mode: RpcMode::Bidirectional,
                    payload: PayloadShape::ProtobufMessage,
                },
            ],
        })
    }
}
impl Service<ServerContext, Request<Input>> for Producer {
    type Response = Response<ProtobufStream<EchoRequest>>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        request: Request<Input>,
    ) -> Result<Self::Response, Status> {
        let cx = request.extensions().get::<StreamContext>().unwrap().clone();
        let mut input = request.into_inner().0;
        assert!(input.message().await?.is_some());
        assert!(input.message().await?.is_none());
        // Model an opening layer that updates the context after admission.
        cx.update_metainfo(|info| info.set_persistent("layer", "latest"));
        let resume = self.0.clone();
        response::protobuf(cx, Default::default(), move |_, mut output| async move {
            assert_eq!(
                METAINFO
                    .with(|info| info.borrow().get_persistent("layer"))
                    .as_deref(),
                Some("latest")
            );
            METAINFO.with(|info| info.borrow_mut().set_backward_transient("phase", "header"));
            output
                .send(&EchoRequest {
                    message: "reply".into(),
                })
                .await?;
            resume.notified().await;
            METAINFO.with(|info| info.borrow_mut().set_backward_transient("phase", "trailer"));
            Ok(())
        })
        .await
    }
}

#[tokio::test]
async fn server_streaming_and_bidi_publish_handler_metainfo_on_the_wire() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let resume = Arc::new(Notify::new());
        let service = ServiceBuilder::new(Producer(resume.clone()))
            .build::<Input, ProtobufStream<EchoRequest>>();
        let (shutdown, stop) = tokio::sync::watch::channel(());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let io = PeekedConn::peek8(socket.into()).await.unwrap().unwrap();
            let router = Router::new().add_service(service);
            serve_detected(
                io,
                MetaService::new(router, DefaultProvider),
                Default::default(),
                stop,
            )
            .await
            .unwrap();
        });
        let (mut client, connection) =
            h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap())
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        for method in ["Watch", "Chat"] {
            let request = http::Request::builder()
                .method("POST")
                .uri(format!("http://{address}/test.Producer/{method}"))
                .header("content-type", "application/grpc")
                .body(())
                .unwrap();
            let (response, mut send) = client.send_request(request, false).unwrap();
            send.send_data(Bytes::from_static(&[0; 5]), true).unwrap();
            let response = response.await.unwrap();
            assert_eq!(response.headers()["rpc-backward-phase"], "header");
            resume.notify_one();
            let mut body = response.into_body();
            while let Some(data) = body.data().await {
                let data = data.unwrap();
                body.flow_control().release_capacity(data.len()).unwrap();
            }
            let trailers = body.trailers().await.unwrap().unwrap();
            assert_eq!(trailers["grpc-status"], "0");
            assert_eq!(trailers["rpc-backward-phase"], "trailer");
        }
        shutdown.send(()).unwrap();
        drop(client);
        server.await.unwrap();
        driver.abort();
    })
    .await
    .expect("response metainfo handoff stalled");
}
