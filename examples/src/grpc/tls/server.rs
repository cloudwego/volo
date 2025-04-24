//! Run with `cargo run --bin hello-tls-grpc-server --features tls`

use std::net::SocketAddr;

use volo::net::tls::ServerTlsConfig;
use volo_grpc::server::{Server, ServiceBuilder};

pub struct S;

impl volo_gen::proto_gen::helloworld::Greeter for S {
    async fn say_hello(
        &self,
        req: volo_grpc::Request<volo_gen::proto_gen::helloworld::HelloRequest>,
    ) -> Result<volo_grpc::Response<volo_gen::proto_gen::helloworld::HelloReply>, volo_grpc::Status>
    {
        let resp = volo_gen::proto_gen::helloworld::HelloReply {
            message: format!("Hello, {}!", req.get_ref().name).into(),
        };
        Ok(volo_grpc::Response::new(resp))
    }
}

#[volo::main]
async fn main() {
    // TLS configuration
    //
    // The key and CertificateDer are copied from
    // https://github.com/hyperium/tonic/tree/master/examples/data/tls
    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let tls_config = ServerTlsConfig::from_pem_file(
        data_dir.join("tls/server.pem"),
        data_dir.join("tls/server.key"),
    )
    .expect("failed to load certs");

    // Server address
    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new()
        .tls_config(tls_config)
        .add_service(
            ServiceBuilder::new(volo_gen::proto_gen::helloworld::GreeterServer::new(S)).build(),
        )
        .run(addr)
        .await
        .unwrap();
}
