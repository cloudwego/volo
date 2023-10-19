use std::net::SocketAddr;

use tokio::task;
use volo::net::Address;
use volo_grpc::server::{Server, ServiceBuilder};

pub struct S {
    addr: Address,
}

impl S {
    pub fn new(addr: Address) -> Self {
        Self { addr }
    }
}

#[volo::async_trait]
impl volo_gen::proto_gen::hello::Greeter for S {
    async fn say_hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::hello::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::hello::HelloReply>, volo_grpc::Status>
    {
        let resp = volo_gen::proto_gen::hello::HelloReply {
            message: format!("Hello, {}!  from {}", req.get_ref().name, self.addr).into(),
        };
        Ok(volo_grpc::Response::new(resp))
    }
}

#[volo::main]
async fn main() {
    println!("start server");
    let addr1: SocketAddr = "[::]:8080".parse().unwrap();
    let addr1 = volo::net::Address::from(addr1);
    let addr2: SocketAddr = "[::]:8081".parse().unwrap();
    let addr2 = volo::net::Address::from(addr2);

    let handle1 = task::spawn(async move {
        Server::new()
            .add_service(
                ServiceBuilder::new(volo_gen::proto_gen::hello::GreeterServer::new(S::new(
                    addr1.clone(),
                )))
                .build(),
            )
            .run(addr1)
            .await
            .unwrap();
    });
    let handle2 = task::spawn(async move {
        Server::new()
            .add_service(
                ServiceBuilder::new(volo_gen::proto_gen::hello::GreeterServer::new(S::new(
                    addr2.clone(),
                )))
                .build(),
            )
            .run(addr2)
            .await
            .unwrap();
    });
    let (_result1, _result2) = tokio::join!(handle1, handle2);

    println!("shutdown");
    // println!("{} {}", result1.unwrap(), result2.unwrap());
}
