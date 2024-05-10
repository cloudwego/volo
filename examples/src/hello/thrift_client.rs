use std::net::SocketAddr;

use lazy_static::lazy_static;
use volo_thrift::client::CallOpt;

lazy_static! {
    static ref CLIENT: volo_gen::thrift_gen::hello::HelloServiceClient = {
        let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
        volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
            .address(addr)
            .build()
    };
}

pub struct LogService<S>(S);

#[volo::main]
async fn main() {
    let req = volo_gen::thrift_gen::hello::HelloRequest {
        name: "volo".into(),
        common: None,
        common2: None,
    };
    let resp = CLIENT
        .clone()
        .with_callopt(CallOpt::default())
        .hello(req)
        .await;
    match resp {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("{e:?}"),
    }
}
