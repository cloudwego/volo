use std::{net::SocketAddr, sync::LazyLock, time::Duration};

use pilota::FastStr;
use tokio::time::sleep;
use volo_grpc::server::{Server, ServiceBuilder, layer::timeout::TimeoutLayer};

static B_CLIENT: LazyLock<volo_gen::proto_gen::helloworld::GreeterClient> = LazyLock::new(|| {
    // Service B
    let addr: SocketAddr = "127.0.0.1:8082".parse().unwrap();
    volo_gen::proto_gen::helloworld::GreeterClientBuilder::new("hello-b")
        .address(addr)
        .rpc_timeout(Some(Duration::from_secs(2)))
        .build()
});

pub struct ServiceA;

impl volo_gen::proto_gen::helloworld::Greeter for ServiceA {
    async fn say_hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::helloworld::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::helloworld::HelloReply>, volo_grpc::Status>
    {
        sleep(Duration::from_secs(2)).await;
        println!("Service A received: {:?}", req.get_ref().name);

        let forwarded_req = volo_gen::proto_gen::helloworld::HelloRequest {
            name: FastStr::from(format!("{} (via A)", req.get_ref().name)),
        };

        let resp = B_CLIENT.say_hello(forwarded_req).await?;
        println!("Service A got from B: {:?}", resp.get_ref().message);

        let wrapped_msg = format!("Service A wrapping: {}", resp.get_ref().message);

        Ok(volo_grpc::Response::new(
            volo_gen::proto_gen::helloworld::HelloReply {
                message: FastStr::from(wrapped_msg),
            },
        ))
    }
}

#[volo::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new()
        .layer(TimeoutLayer::new())
        .add_service(
            ServiceBuilder::new(volo_gen::proto_gen::helloworld::GreeterServer::new(
                ServiceA,
            ))
            .build(),
        )
        .run(addr)
        .await
        .unwrap();
}
