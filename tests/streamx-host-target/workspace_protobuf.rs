use common_types::boundary::common::Value;
use volo_streamx::{ResponseSender, RpcResult, StreamContext};

struct Handler;
impl api_types::boundary::api::presence_streamx::Handler for Handler {
    async fn get(&self, _: StreamContext, value: Value) -> RpcResult<Value> {
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
    volo_streamx::Server::new()
        .add_service(api_types::boundary::api::presence_streamx::bind(Handler))
}
