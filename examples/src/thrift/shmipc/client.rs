use std::sync::LazyLock;

use volo_thrift::client::CallOpt;

static CLIENT: LazyLock<volo_gen::thrift_gen::hello::HelloServiceClient> = LazyLock::new(|| {
    let uds_path = std::os::unix::net::SocketAddr::from_pathname("/tmp/hello_test.sock").unwrap();
    volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(volo::net::ShmipcAddr(uds_path))
        .build()
});

#[volo::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let config = volo::net::shmipc::config::Config {
        share_memory_path_prefix: "/dev/shm/client.ipc.shm".to_string(),
        mem_map_type: volo::net::shmipc::config::MemMapType::MemMapTypeMemFd,
        ..Default::default()
    };
    volo::net::shmipc::config::DEFAULT_SHMIPC_CONFIG.store(config.into());

    let desc = volo_gen::thrift_gen::hello::HelloRequest::get_descriptor()
        .unwrap()
        .type_descriptor();
    println!("{desc:?}");

    loop {
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
            Err(e) => eprintln!("{e:?}"),
        }
        // tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
