use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use streamx_codegen::thrift::chat::{
    ArchiveServiceClientBuilder, ArchiveServiceGetArgsRecv, ArchiveServiceGetResultRecv, Message,
    archive_service_streamx,
};
use volo::net::incoming::DefaultIncoming;
use volo_streamx::{ConnectionEvent, Protocol, Protocols, Rejection, RpcResult, StreamContext};

struct Archive(Arc<AtomicUsize>);
impl archive_service_streamx::Handler for Archive {
    async fn get(&self, _: StreamContext, request: Message) -> RpcResult<Message> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(request)
    }
}
fn message() -> Message {
    Message {
        text: "unary".into(),
        sequence: 0,
    }
}
fn ordinary_request() -> bytes::Bytes {
    use pilota::thrift::{Message as _, TOutputProtocol};
    let mut bytes = pilota::LinkedBytes::new();
    let mut output = pilota::thrift::binary::TBinaryProtocol::new(&mut bytes, true);
    output
        .write_message_begin(&pilota::thrift::TMessageIdentifier::new(
            "Get".into(),
            pilota::thrift::TMessageType::Call,
            9,
        ))
        .unwrap();
    ArchiveServiceGetArgsRecv { request: message() }
        .encode(&mut output)
        .unwrap();
    output.write_message_end().unwrap();
    bytes.into_bytes_mut().freeze()
}

#[tokio::test]
async fn unary_only_binding_uses_two_drivers_and_classification_stays_fixed() {
    use pilota::thrift::{Message as _, TAsyncInputProtocol};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(Duration::from_secs(5), async {
        let calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(archive_service_streamx::bind(Archive(calls.clone())))
            .connection_observer(move |event| observed.lock().unwrap().push(event))
            .graceful_shutdown_timeout(Duration::from_millis(300))
            .prepare()
            .unwrap();
        assert_eq!(prepared.protocols(), Protocols::THRIFT | Protocols::GRPC);
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let thrift = ArchiveServiceClientBuilder::new("archive")
            .address(address)
            .build();
        assert_eq!(thrift.get(message()).await.unwrap().text, "unary");
        let grpc = streamx_codegen::grpc::chat::ArchiveServiceClientBuilder::new("archive")
            .address(address)
            .build();
        let response = grpc
            .get(streamx_codegen::grpc::chat::ArchiveServiceGetRequest { request: message() })
            .await
            .unwrap();
        assert_eq!(response.into_inner().text, "unary");
        let mut disabled = tokio::net::TcpStream::connect(address).await.unwrap();
        disabled
            .write_all(&[0, 0, 0, 0, 0x10, 0, 0, 2])
            .await
            .unwrap();
        assert_eq!(disabled.read(&mut [0; 1]).await.unwrap(), 0);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket.write_all(&ordinary_request()).await.unwrap();
        {
            let mut input = pilota::thrift::TAsyncBinaryProtocol::new(&mut socket);
            assert_eq!(input.read_message_begin().await.unwrap().sequence_number, 9);
            ArchiveServiceGetResultRecv::decode_async(&mut input)
                .await
                .unwrap();
        }
        socket
            .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            .await
            .unwrap();
        let _ = socket.read_to_end(&mut Vec::new()).await;
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        handle.shutdown();
        server.await.unwrap().unwrap();
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    ConnectionEvent::Selected {
                        protocol: Protocol::Thrift,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    ConnectionEvent::Selected {
                        protocol: Protocol::Grpc,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    ConnectionEvent::Rejected(Rejection::DisabledProtocol)
                ))
                .count(),
            1
        );
        assert_eq!(handle.stats().detected, 4);
        assert_eq!(handle.stats().accepted, 4);
        assert_eq!(handle.stats().active, 0);
    })
    .await
    .unwrap();
}
