use std::time::Duration;

use streamx_codegen::{name_edges_pb as pb, thrift::chat::*};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use volo::net::incoming::DefaultIncoming;
use volo_streamx::{RequestStream, ResponseSender, RpcResult, StreamContext};

struct Global;
impl pb::global_proto_streamx::Handler for Global {
    async fn r#match(&self, _: StreamContext, value: pb::Value) -> RpcResult<pb::Value> {
        Ok(value)
    }
    async fn watch(
        &self,
        _: StreamContext,
        value: pb::Value,
        mut output: ResponseSender<pb::Value>,
    ) -> RpcResult<()> {
        output.send(&value).await
    }
}

#[tokio::test]
async fn no_package_proto_generated_client_matches_streamx_wire_path() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let prepared = volo_streamx::Server::new()
            .add_service(pb::global_proto_streamx::bind(Global))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let client = pb::GlobalProtoClientBuilder::new("global")
            .address(address)
            .build();
        let value = pb::Value {
            text: "no package".into(),
        };
        assert_eq!(
            client.r#match(value.clone()).await.unwrap().into_inner(),
            value
        );
        let mut stream = client.watch(value.clone()).await.unwrap().into_inner();
        use futures::StreamExt;
        assert_eq!(stream.next().await.unwrap().unwrap(), value);
        assert!(stream.next().await.is_none());
        handle.shutdown();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn prefix_timeout_releases_incomplete_connections() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = events.clone();
        let prepared = volo_streamx::Server::new()
            .add_service(pb::global_proto_streamx::bind(Global))
            .add_fallback_service(chat_service_streamx::bind(Unused))
            .connection_timeout(Duration::from_millis(30))
            .connection_observer(move |event| observed.lock().unwrap().push(event))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        for prefix in [b"".as_slice(), b"PRI * H"] {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(prefix).await.unwrap();
            assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
        }
        assert_eq!(handle.stats().active, 0);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(
                    event,
                    volo_streamx::ConnectionEvent::Rejected(volo_streamx::Rejection::Timeout)
                ))
                .count(),
            2
        );
        handle.shutdown();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

// A syntactically valid Compact request with an unknown one-character method
// must receive UNKNOWN_METHOD, rather than wait for two bytes of the response.
struct Unused;
impl chat_service_streamx::Handler for Unused {
    async fn get(
        &self,
        _: StreamContext,
        _: Message,
    ) -> RpcResult<Result<Message, ChatServiceGetException>> {
        unreachable!()
    }
    async fn ping(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        unreachable!()
    }
    async fn add(&self, _: StreamContext, _: ChatServiceAddArgsRecv) -> RpcResult<i64> {
        unreachable!()
    }
    async fn upload(&self, _: StreamContext, _: RequestStream<Message>) -> RpcResult<Message> {
        unreachable!()
    }
    async fn watch(
        &self,
        _: StreamContext,
        _: Message,
        _: ResponseSender<Message>,
    ) -> RpcResult<()> {
        unreachable!()
    }
    async fn talk(
        &self,
        _: StreamContext,
        _: RequestStream<Message>,
        _: ResponseSender<Message>,
    ) -> RpcResult<()> {
        unreachable!()
    }
    async fn fire(&self, _: StreamContext, _: Message) -> RpcResult<()> {
        unreachable!()
    }
}
#[tokio::test]
async fn six_byte_buffered_compact_request_receives_a_response() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let prepared = volo_streamx::Server::new()
            .add_fallback_service(chat_service_streamx::bind(Unused))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(&[0x82, 0x21, 0, 1, b'm', 0])
            .await
            .unwrap();
        let mut first = [0; 1];
        stream.read_exact(&mut first).await.unwrap();
        assert_eq!(first, [0x82]);
        handle.shutdown();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
