include!(concat!(env!("OUT_DIR"), "/thrift/generated.rs"));
pub mod thrift {
    pub use super::generated::*;
}
pub mod protobuf {
    include!(concat!(env!("OUT_DIR"), "/protobuf/generated.rs"));
    pub use generated::*;
}

use volo_streamx::{ResponseSender, RpcResult, StreamContext};

struct Thrift;
impl thrift::boundary_thrift::echo_streamx::Handler for Thrift {
    async fn inherited(
        &self,
        _: StreamContext,
        value: thrift::boundary_parent::Value,
    ) -> RpcResult<thrift::boundary_parent::Value> {
        Ok(value)
    }
    async fn watch(
        &self,
        _: StreamContext,
        value: thrift::boundary_parent::Value,
        mut output: ResponseSender<thrift::boundary_parent::Value>,
    ) -> RpcResult<()> {
        output.send(&value).await
    }
}

struct Protobuf;
impl protobuf::boundary::api::presence_streamx::Handler for Protobuf {
    async fn get(
        &self,
        _: StreamContext,
        value: protobuf::boundary::common::Value,
    ) -> RpcResult<protobuf::boundary::common::Value> {
        Ok(value)
    }
    async fn watch(
        &self,
        _: StreamContext,
        value: protobuf::boundary::common::Value,
        mut output: ResponseSender<protobuf::boundary::common::Value>,
    ) -> RpcResult<()> {
        output.send(&value).await
    }
}

pub fn server() -> volo_streamx::Server {
    volo_streamx::Server::new()
        .add_service(thrift::boundary_thrift::echo_streamx::bind(Thrift))
        .add_service(protobuf::boundary::api::presence_streamx::bind(Protobuf))
}
