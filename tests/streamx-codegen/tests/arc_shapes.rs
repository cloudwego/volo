use std::sync::Arc;

use streamx_codegen::arc_shapes_thrift::arc_shapes::*;
use volo_streamx::{RpcResult, StreamContext};
struct Handler;
impl arc_shapes_streamx::Handler for Handler {
    async fn scalar(&self, _: StreamContext, value: i32) -> RpcResult<i32> {
        Ok(value)
    }
    async fn numbers(&self, _: StreamContext, value: Vec<i32>) -> RpcResult<Vec<i32>> {
        Ok(value)
    }
    async fn messages(
        &self,
        _: StreamContext,
        value: Vec<Arc<Message>>,
    ) -> RpcResult<Vec<Arc<Message>>> {
        Ok(value)
    }
    async fn shadow(&self, _: StreamContext, value: Arc<S>) -> RpcResult<Arc<S>> {
        Ok(value)
    }
    async fn empty(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        Ok(())
    }
}
#[tokio::test]
async fn service_arc_policy_preserves_lowered_scalar_and_container_fields() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        assert_eq!(arc_shapes_streamx::DESCRIPTOR.wire_name, "c.ArcShapes");
        let prepared = volo_streamx::Server::new()
            .add_service(arc_shapes_streamx::bind(Handler))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            tokio::spawn(prepared.run(volo::net::incoming::DefaultIncoming::from(listener)));
        let native = ArcShapesClientBuilder::new("arc").address(address).build();
        assert_eq!(native.scalar(42).await.unwrap(), 42);
        assert_eq!(native.numbers(vec![1, 2]).await.unwrap(), vec![1, 2]);
        let value = vec![Arc::new(Message {
            text: Some("value".into()),
        })];
        assert_eq!(native.messages(value.clone()).await.unwrap(), value);
        native.empty().await.unwrap();
        let shadow = Arc::new(S { value: Some(42) });
        assert_eq!(native.shadow(shadow.clone()).await.unwrap(), shadow);
        use streamx_codegen::arc_shapes_grpc::arc_shapes as grpc;
        let client = grpc::ArcShapesClientBuilder::new("arc")
            .address(address)
            .build();
        assert_eq!(
            client
                .scalar(grpc::ArcShapesScalarRequest { value: 42 })
                .await
                .unwrap()
                .into_inner(),
            42
        );
        assert_eq!(
            client
                .numbers(grpc::ArcShapesNumbersRequest { value: vec![1, 2] })
                .await
                .unwrap()
                .into_inner(),
            vec![1, 2]
        );
        handle.shutdown();
        server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
