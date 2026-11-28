use std::{sync::Arc, time::Duration};

use streamx_codegen::thrift::chat::*;
use volo::net::incoming::DefaultIncoming;
use volo_streamx::{RequestStream, ResponseSender, RpcResult, StreamContext};

struct Shared;
impl shared_service_streamx::Handler for Shared {
    async fn shared(&self, _: StreamContext, request: Arc<Message>) -> RpcResult<Arc<Message>> {
        Ok(request)
    }
    async fn shared_upload(
        &self,
        _: StreamContext,
        mut input: RequestStream<Arc<Message>>,
    ) -> RpcResult<Arc<Message>> {
        let mut count = 0;
        while input.recv().await?.is_some() {
            count += 1;
        }
        Ok(Arc::new(Message {
            text: "upload".into(),
            sequence: count,
        }))
    }
    async fn shared_watch(
        &self,
        _: StreamContext,
        request: Arc<Message>,
        mut output: ResponseSender<Arc<Message>>,
    ) -> RpcResult<()> {
        output.send(&request).await
    }
    async fn shared_talk(
        &self,
        _: StreamContext,
        mut input: RequestStream<Arc<Message>>,
        mut output: ResponseSender<Arc<Message>>,
    ) -> RpcResult<()> {
        while let Some(request) = input.recv().await? {
            output.send(&request).await?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn arc_arguments_keep_their_declared_types_across_all_three_drivers() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let prepared = volo_streamx::Server::new()
            .add_service(shared_service_streamx::bind(Shared))
            .graceful_shutdown_timeout(Duration::from_millis(500))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let native = SharedServiceClientBuilder::new("shared")
            .address(address)
            .build();
        let grpc = streamx_codegen::grpc::chat::SharedServiceClientBuilder::new("shared")
            .address(address)
            .build();
        let request = Arc::new(Message {
            text: "shared".into(),
            sequence: 7,
        });
        assert_eq!(native.shared(request.clone()).await.unwrap(), request);
        assert_eq!(
            grpc.shared(streamx_codegen::grpc::chat::SharedServiceSharedRequest {
                request: request.clone()
            })
            .await
            .unwrap()
            .into_inner(),
            request
        );

        let mut upload = native.shared_upload().await.unwrap();
        upload.send(&request).await.unwrap();
        assert_eq!(upload.close_and_recv().await.unwrap().sequence, 1);
        assert_eq!(
            grpc.shared_upload(futures::stream::iter([request.clone()]))
                .await
                .unwrap()
                .into_inner()
                .sequence,
            1
        );

        let mut watch = native.shared_watch(request.clone()).await.unwrap();
        assert_eq!(watch.recv().await.unwrap().unwrap(), request);
        assert!(watch.recv().await.unwrap().is_none());
        let mut watch = grpc
            .shared_watch(request.clone())
            .await
            .unwrap()
            .into_inner();
        assert_eq!(watch.message().await.unwrap().unwrap(), request);
        assert!(watch.message().await.unwrap().is_none());

        let (mut send, mut recv) = native.shared_talk().await.unwrap().into_split();
        send.send(&request).await.unwrap();
        send.close_send().await.unwrap();
        assert_eq!(recv.recv().await.unwrap().unwrap(), request);
        assert!(recv.recv().await.unwrap().is_none());
        let mut talk = grpc
            .shared_talk(futures::stream::iter([request.clone()]))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(talk.message().await.unwrap().unwrap(), request);
        assert!(talk.message().await.unwrap().is_none());
        handle.shutdown();
        server.await.unwrap().unwrap();
        assert_eq!(handle.stats().active_calls, 0);
    })
    .await
    .unwrap();
}
