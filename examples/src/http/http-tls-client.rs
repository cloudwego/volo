use volo::net::tls::TlsConnector;
use volo_http::{body::BodyConversion, client::Client};

#[volo::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let data_dir = std::path::PathBuf::from_iter([std::env!("CARGO_MANIFEST_DIR"), "data"]);
    let connector = TlsConnector::builder()
        .enable_default_root_certs(false)
        .add_pem_from_file(data_dir.join("tls/ca.pem"))
        .expect("failed to read ca.pem")
        .build()
        .expect("failed to build TlsConnector");

    let client = {
        let mut builder = Client::builder();
        builder.set_tls_config(connector);
        builder.build()
    };

    let resp = client
        .get("https://[::1]:8080/")
        .expect("invalid uri")
        .send()
        .await
        .expect("request failed")
        .into_string()
        .await
        .expect("response failed to convert to string");

    println!("{resp}");
}
