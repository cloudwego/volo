#![cfg(all(feature = "ttstream", feature = "shmipc", feature = "multiplex"))]
use std::time::Duration;

use ttstream_codegen::volo_gen::interop::{Interop, InteropClientBuilder, InteropServer, Message};
use volo::net::{
    Address,
    incoming::DefaultIncoming,
    shmipc::{
        self,
        config::{
            Config, MemMapType, SESSION_MANAGER_CONFIG, SessionManagerConfig, SizePercentPair,
        },
    },
};
use volo_thrift::streaming::{RequestStream, ResponseSender, StreamContext, StreamError};
struct Echo;
impl Interop for Echo {
    async fn unary(&self, req: Message) -> Result<Message, volo_thrift::ServerError> {
        Ok(req)
    }
    async fn explicit_unary(&self, req: Message) -> Result<Message, volo_thrift::ServerError> {
        Ok(req)
    }
    async fn client(
        &self,
        _: StreamContext,
        mut input: RequestStream<Message>,
    ) -> Result<Message, StreamError> {
        Ok(input.recv().await?.unwrap_or_default())
    }
    async fn server(
        &self,
        _: StreamContext,
        request: Message,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        output.send(&request).await
    }
    async fn bidi(
        &self,
        _: StreamContext,
        mut input: RequestStream<Message>,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        while let Some(request) = input.recv().await? {
            output.send(&request).await?;
        }
        Ok(())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ttstream_shmipc_is_not_rejected_by_unary_multiplex_configuration() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let socket =
            std::os::unix::net::SocketAddr::from_pathname(dir.path().join("control.sock")).unwrap();
        let config = Config {
            queue_path: dir.path().join("queue").to_string_lossy().into_owned(),
            share_memory_path_prefix: dir.path().join("buffer").to_string_lossy().into_owned(),
            share_memory_buffer_cap: 4 << 20,
            buffer_slice_sizes: vec![SizePercentPair {
                size: 4096,
                percent: 100,
            }],
            mem_map_type: MemMapType::MemMapTypeDevShmFile,
            ..Default::default()
        };
        let listener =
            shmipc::Listener::listen(shmipc::Address::Unix(socket.clone()), Some(config.clone()))
                .await
                .unwrap();
        let server = tokio::spawn(
            InteropServer::new(Echo)
                .multiplex(true)
                .run(DefaultIncoming::from(listener)),
        );
        SESSION_MANAGER_CONFIG
            .scope(
                SessionManagerConfig::default()
                    .with_config(config)
                    .with_session_num(1),
                async {
                    let client = InteropClientBuilder::new("Interop")
                        .address(Address::Shmipc(shmipc::Address::Unix(socket)))
                        .build();
                    let (mut tx, mut rx) = client.bidi().await.unwrap().into_split();
                    for seq in 0..3 {
                        tx.send(&Message {
                            value: "actual shared-memory".into(),
                            seq,
                        })
                        .await
                        .unwrap();
                        let response = rx.recv().await.unwrap().unwrap();
                        assert_eq!(response.seq, seq);
                    }
                    tx.close_send().await.unwrap();
                    assert!(rx.recv().await.unwrap().is_none());
                },
            )
            .await;
        server.abort();
    })
    .await
    .unwrap();
}
