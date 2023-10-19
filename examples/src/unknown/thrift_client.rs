use std::net::SocketAddr;

use lazy_static::lazy_static;
use volo_thrift::client::CallOpt;

lazy_static! {
    static ref CLIENT: volo_gen::thrift_gen::echo::EchoServiceClient = {
        let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
        volo_gen::thrift_gen::echo::EchoServiceClientBuilder::new("hello")
            .address(addr)
            .build()
    };
}

pub struct LogService<S>(S);

#[volo::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let req = volo_gen::thrift_gen::echo::EchoRequest {
        name: "volo".into(),
        faststr: "faststr".into(),
        faststr_with_default: "faststr_with_default".into(),
        map: Default::default(),
        map_with_default: Default::default(),
        echo_union: volo_gen::thrift_gen::echo::EchoUnion::A(true),
        echo_enum: Default::default(),
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
