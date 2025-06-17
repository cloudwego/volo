use std::{net::SocketAddr, sync::LazyLock};

use volo_thrift::client::CallOpt;

static CLIENT: LazyLock<volo_gen::thrift_gen::echo::EchoServiceClient> = LazyLock::new(|| {
    let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
    volo_gen::thrift_gen::echo::EchoServiceClientBuilder::new("hello")
        .address(addr)
        .build()
});

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
        _field_mask: None,
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
