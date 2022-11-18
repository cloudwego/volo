#![feature(type_alias_impl_trait)]

use std::net::SocketAddr;

use lazy_static::lazy_static;
use volo_grpc::codec::compression::{CompressionConfig, CompressionEncoding};

lazy_static! {
    static ref CLIENT: volo_gen::proto_gen::hello::HelloServiceClient = {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::hello::HelloServiceClientBuilder::new("hello")
            .send_compression(CompressionConfig {
                encoding: CompressionEncoding::Gzip,
                level: 6,
            })
            .accept_compression(CompressionEncoding::Gzip)
            .address(addr)
            .build()
    };
}

#[volo::main]
async fn main() {
    let req = volo_gen::proto_gen::hello::HelloRequest {
        name: "Volo".to_string(),
    };
    let resp = CLIENT.clone().hello(req).await;
    match resp {
        Ok(info) => println!("{:?}", info),
        Err(e) => eprintln!("{:?}", e),
    }
}
