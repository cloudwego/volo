use std::sync::LazyLock;

use volo_thrift::client::CallOpt;

const PIPE_NAME: &str = r"\\.\pipe\volo_thrift_named_pipe_example";

static CLIENT: LazyLock<volo_gen::thrift_gen::hello::HelloServiceClient> = LazyLock::new(|| {
    volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(volo::net::Address::from(PIPE_NAME))
        .build()
});

#[volo::main]
async fn main() {
    tracing_subscriber::fmt::init();

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
        Err(err) => eprintln!("{err:?}"),
    }
}
