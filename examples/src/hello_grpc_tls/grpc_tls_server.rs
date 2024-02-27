//! Run with `cargo run --bin hello-tls-grpc-server --features tls`

use std::{net::SocketAddr, path::Path, sync::Arc};

use rustls::ServerConfig;
use rustls_pemfile::{certs, pkcs8_private_keys};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use volo_grpc::{
    server::{Server, ServiceBuilder},
    transport::ServerTlsConfig,
}; // crate `rustls` is renamed to `librustls` in this example

pub struct S;

impl volo_gen::proto_gen::hello::Greeter for S {
    async fn say_hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::hello::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::hello::HelloReply>, volo_grpc::Status>
    {
        let resp = volo_gen::proto_gen::hello::HelloReply {
            message: format!("Hello, {}!", req.get_ref().name).into(),
        };
        Ok(volo_grpc::Response::new(resp))
    }
}

fn load_certs(path: impl AsRef<Path>) -> std::io::Result<Vec<CertificateDer<'static>>> {
    Ok(
        certs(&mut std::io::BufReader::new(std::fs::File::open(path)?))
            .map(|v| v.unwrap())
            .collect::<Vec<_>>(),
    )
}

fn load_keys(path: impl AsRef<Path>) -> std::io::Result<Vec<PrivateKeyDer<'static>>> {
    Ok(
        pkcs8_private_keys(&mut std::io::BufReader::new(std::fs::File::open(path)?))
            .map(|v| PrivateKeyDer::Pkcs8(v.unwrap()))
            .collect::<Vec<_>>(),
    )
}

#[volo::main]
async fn main() {
    // TLS configuration
    //
    // The key and CertificateDer are copied from
    // https://github.com/hyperium/tonic/tree/master/examples/data/tls
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let certs = load_certs(data_dir.join("tls/server.pem")).unwrap();
    let private_key = load_keys(data_dir.join("tls/server.key"))
        .unwrap()
        .remove(0);

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, private_key)
        .expect("bad CertificateDer/key");
    server_config.alpn_protocols = vec![b"h2".to_vec()];

    let server_config = Arc::new(server_config);
    let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
    let tls_config = ServerTlsConfig::from(acceptor);

    // Server address
    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new()
        .tls_config(tls_config)
        .add_service(ServiceBuilder::new(volo_gen::proto_gen::hello::GreeterServer::new(S)).build())
        .run(addr)
        .await
        .unwrap();
}
