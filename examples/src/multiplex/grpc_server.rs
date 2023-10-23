use std::net::SocketAddr;

use volo_grpc::server::{Server, ServiceBuilder};

pub struct G;

impl volo_gen::proto_gen::hello::Greeter for G {
    async fn say_hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::hello::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::hello::HelloReply>, volo_grpc::Status>
    {
        let resp = volo_gen::proto_gen::hello::HelloReply {
            message: format!("Hello, {}!", req.get_ref().name).into(),
        };
        Ok(volo_grpc::Response::new(resp))
    }
}

pub struct E;

impl volo_gen::proto_gen::echo::Echo for E {
    async fn echo(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::echo::EchoRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::echo::EchoResponse>, volo_grpc::Status>
    {
        let resp = volo_gen::proto_gen::echo::EchoResponse {
            message: req.get_ref().message.to_string().into(),
        };
        Ok(volo_grpc::Response::new(resp))
    }
}

#[volo::main]
async fn main() {
    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new()
        .add_service(ServiceBuilder::new(volo_gen::proto_gen::hello::GreeterServer::new(G)).build())
        .add_service(ServiceBuilder::new(volo_gen::proto_gen::echo::EchoServer::new(E)).build())
        .run(addr)
        .await
        .unwrap();
}
