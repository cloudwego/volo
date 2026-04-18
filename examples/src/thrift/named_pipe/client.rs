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

    let desc = volo_gen::thrift_gen::hello::HelloRequest::get_descriptor()
        .unwrap()
        .type_descriptor();
    println!("{desc:?}");

    let fm = pilota_thrift_fieldmask::FieldMaskBuilder::new(&desc, &["$.hello"])
        .with_options(pilota_thrift_fieldmask::Options::new().with_black_list_mode(true))
        .build()
        .unwrap();
    println!("{fm:?}");

    let mut req = volo_gen::thrift_gen::hello::HelloRequest {
        name: "volo".into(),
        hello: Some("world".into()),
        _field_mask: None,
    };
    req.set_field_mask(fm);

    println!("req with field mask: {req:?}");
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
