//! Run with `cargo run --bin hello-tls-grpc-client --features tls`

use std::net::SocketAddr;

use faststr::FastStr;
use volo::net::tls::{ClientTlsConfig, TlsConnector};

#[volo::main]
async fn main() {
    // The key and CertificateDer are copied from
    // https://github.com/hyperium/tonic/tree/master/examples/data/tls
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let connector = TlsConnector::builder()
        .enable_default_root_certs(false)
        .add_pem_from_file(data_dir.join("tls/ca.pem"))
        .expect("failed to read ca.pem")
        .build()
        .expect("failed to build TlsConnector");
    let tls_config = ClientTlsConfig::new("example.com", connector);

    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let client = volo_gen::proto_gen::hello::GreeterClientBuilder::new("hello")
        .tls_config(tls_config)
        .address(addr)
        .build();

    let req = volo_gen::proto_gen::hello::HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };
    let resp = client.say_hello(req).await;
    match resp {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("{e:?}"),
    }
}
