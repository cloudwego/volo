//! Run with `cargo run --example hello-tls-grpc-client --features tls`

use std::{net::SocketAddr, path::Path, sync::Arc};

use librustls::{Certificate, ClientConfig, RootCertStore}; /* crate `rustls` is renamed to `librustls` in this example */
use pilota::FastStr;
use rustls_pemfile::certs;
use volo::net::dial::ClientTlsConfig;

fn load_certs(path: impl AsRef<Path>) -> std::io::Result<Vec<Certificate>> {
    certs(&mut std::io::BufReader::new(std::fs::File::open(path)?))
        .map(|v| v.into_iter().map(Certificate).collect())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid cert").into())
}

#[volo::main]
async fn main() {
    // The key and certificate are copied from
    // https://github.com/hyperium/tonic/tree/master/examples/data/tls
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let root_cert = load_certs(data_dir.join("tls/ca.pem")).unwrap();
    let mut root_certs = RootCertStore::empty();
    root_certs.add(&root_cert[0]).unwrap();

    let client_config = ClientConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_certs)
        .with_no_client_auth();
    let client_config = Arc::new(client_config);
    let connector = tokio_rustls::TlsConnector::from(client_config);
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
