use std::{net::SocketAddr, sync::LazyLock};

use volo_thrift::client::CallOpt;

static CLIENT: LazyLock<volo_gen::thrift_gen::hello::HelloServiceClient> = LazyLock::new(|| {
    let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
    volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(addr)
        .build()
});

#[volo::main]
async fn main() {
    let req = volo_gen::thrift_gen::hello::HelloRequest {
        name: "volo".into(),
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
