#![feature(impl_trait_in_assoc_type)]

use std::net::SocketAddr;

use lazy_static::lazy_static;
use pilota::FastStr;
use volo_grpc::codec::compression::{
    CompressionEncoding::{Gzip, Identity, Zlib},
    GzipConfig, Level, ZlibConfig,
};

lazy_static! {
    static ref CLIENT: volo_gen::proto_gen::hello::GreeterClient = {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::hello::GreeterClientBuilder::new("hello")
            .send_compressions(vec![
                Gzip(Some(GzipConfig::default())),
                Zlib(Some(ZlibConfig {
                    level: Level::fast(),
                })),
            ])
            .accept_compressions(vec![Gzip(None), Identity])
            .address(addr)
            .build()
    };
}

#[volo::main]
async fn main() {
    let req = volo_gen::proto_gen::hello::HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };
    let resp = CLIENT.say_hello(req).await;

    match resp {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("{e:?}"),
    }
}
