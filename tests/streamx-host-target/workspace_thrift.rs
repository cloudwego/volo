use parent_types::boundary_parent::Value;
use volo_streamx::{ResponseSender, RpcResult, StreamContext};

struct Handler;
impl api_types::boundary_thrift::echo_streamx::Handler for Handler {
    async fn inherited(&self, _: StreamContext, value: Value) -> RpcResult<Value> {
        Ok(value)
    }
    async fn watch(
        &self,
        _: StreamContext,
        value: Value,
        mut output: ResponseSender<Value>,
    ) -> RpcResult<()> {
        output.send(&value).await
    }
}

pub fn server() -> volo_streamx::Server {
    volo_streamx::Server::new().add_service(api_types::boundary_thrift::echo_streamx::bind(Handler))
}
