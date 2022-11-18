#![feature(type_alias_impl_trait)]

use std::net::SocketAddr;

pub struct S;

#[volo::async_trait]
impl volo_gen::proto_gen::hello::HelloService for S {
    async fn hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::hello::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::hello::HelloResponse>, volo_grpc::Status>
    {
        let resp = volo_gen::proto_gen::hello::HelloResponse {
            message: format!("Hello, {}!", req.get_ref().name),
        };
        Ok(volo_grpc::Response::new(resp))
    }
}

#[volo::main]
async fn main() {
    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    volo_gen::proto_gen::hello::HelloServiceServer::new(S)
        .run(addr)
        .await
        .unwrap();
}
