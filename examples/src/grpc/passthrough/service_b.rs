use std::{net::SocketAddr, time::Duration};

use tokio::time::sleep;
use volo_grpc::server::{layer::timeout::TimeoutLayer, Server, ServiceBuilder};

pub struct ServiceB;

impl volo_gen::proto_gen::helloworld::Greeter for ServiceB {
    async fn say_hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::helloworld::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::helloworld::HelloReply>, volo_grpc::Status>
    {
        sleep(Duration::from_secs(2)).await;
        println!("Service B received: {:?}", req.get_ref().name);

        let reply = volo_gen::proto_gen::helloworld::HelloReply {
            message: format!("Hello from B to {}", req.get_ref().name).into(),
        };
        Ok(volo_grpc::Response::new(reply))
    }
}

#[volo::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let addr: SocketAddr = "127.0.0.1:8082".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new()
        .layer(TimeoutLayer::new())
        .add_service(
            ServiceBuilder::new(volo_gen::proto_gen::helloworld::GreeterServer::new(
                ServiceB,
            ))
            .build(),
        )
        .run(addr)
        .await
        .unwrap();
}
