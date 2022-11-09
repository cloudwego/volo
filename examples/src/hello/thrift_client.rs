#![feature(type_alias_impl_trait)]

use std::net::SocketAddr;

use lazy_static::lazy_static;

lazy_static! {
    static ref CLIENT: volo_gen::thrift_gen::hello::HelloServiceClient = {
        let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
        volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
            .address(addr)
            .build()
    };
}

#[volo::main]
async fn main() {
    let req = volo_gen::thrift_gen::hello::HelloRequest {
        name: "volo".to_string(),
    };
    let resp = CLIENT.clone().hello(req).await;
    match resp {
        Ok(info) => println!("{:?}", info),
        Err(e) => eprintln!("{:?}", e),
    }
}
