use std::{net::SocketAddr, sync::LazyLock};

use pilota::FastStr;
use volo_grpc::codec::compression::{
    CompressionEncoding::{Gzip, Identity, Zlib},
    GzipConfig, Level, ZlibConfig,
};

static CLIENT: LazyLock<volo_gen::proto_gen::helloworld::GreeterClient> = LazyLock::new(|| {
    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    volo_gen::proto_gen::helloworld::GreeterClientBuilder::new("hello")
        .send_compressions(vec![
            Gzip(Some(GzipConfig::default())),
            Zlib(Some(ZlibConfig {
                level: Level::fast(),
            })),
        ])
        .accept_compressions(vec![Gzip(None), Identity])
        .address(addr)
        .build()
});

#[volo::main]
async fn main() {
    let req = volo_gen::proto_gen::helloworld::HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };
    let resp = CLIENT.say_hello(req).await;

    match resp {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("{e:?}"),
    }
}
