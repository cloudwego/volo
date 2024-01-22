//! Run with `cargo run --bin hello-tls-grpc-client --features tls`

use std::{net::SocketAddr, path::Path, sync::Arc};

use librustls::{ClientConfig, RootCertStore}; /* crate `rustls` is renamed to `librustls`
                                                * in this example */
use pilota::FastStr;
use rustls_pemfile::certs;
use rustls_pki_types::CertificateDer;
use volo::net::dial::ClientTlsConfig;

fn load_certs(path: impl AsRef<Path>) -> std::io::Result<Vec<CertificateDer<'static>>> {
    Ok(
        certs(&mut std::io::BufReader::new(std::fs::File::open(path)?))
            .map(|v| v.unwrap())
            .collect::<Vec<_>>(),
    )
}

#[volo::main]
async fn main() {
    // The key and CertificateDer are copied from
    // https://github.com/hyperium/tonic/tree/master/examples/data/tls
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let mut root_cert = load_certs(data_dir.join("tls/ca.pem")).unwrap();
    let mut root_certs = RootCertStore::empty();
    root_certs.add(root_cert.remove(0)).unwrap();

    let client_config = ClientConfig::builder()
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
