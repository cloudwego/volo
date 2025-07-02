//! Test it with:
//!
//! ```bash
//! curl -v --cacert examples/data/tls/ca.pem https://127.0.0.1:8080/
//! ```
//!
//! Or use the tls client directly.

use std::{net::SocketAddr, time::Duration};

use volo::net::tls::ServerTlsConfig;
use volo_http::server::{
    Server,
    layer::TimeoutLayer,
    route::{Router, get},
};

async fn index() -> &'static str {
    "It Works!\n"
}

#[volo::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let tls_config = ServerTlsConfig::from_pem_file(
        data_dir.join("tls/server.pem"),
        data_dir.join("tls/server.key"),
    )
    .expect("failed to load certs");

    let app = Router::new()
        .route("/", get(index))
        .layer(TimeoutLayer::new(Duration::from_secs(5), |_: &_| {
            http::StatusCode::REQUEST_TIMEOUT
        }));

    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    println!("Listening on {addr}");

    Server::new(app)
        .tls_config(tls_config)
        .run(addr)
        .await
        .unwrap();
}
