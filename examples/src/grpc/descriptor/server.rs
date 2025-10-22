use std::net::SocketAddr;

use volo_grpc::server::{Server, ServiceBuilder};

pub struct S;

impl volo_gen::proto_gen::nested::Greeter for S {
    async fn say_hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::nested::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::nested::HelloReply>, volo_grpc::Status>
    {
        let resp = volo_gen::proto_gen::nested::HelloReply {
            message: format!("Hello, {}!", req.get_ref().name).into(),
        };
        Ok(volo_grpc::Response::new(resp))
    }
}

#[volo::main]
async fn main() {
    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new()
        .add_service(
            ServiceBuilder::new(volo_gen::proto_gen::nested::GreeterServer::new(S)).build(),
        )
        .run(addr)
        .await
        .unwrap();
}
