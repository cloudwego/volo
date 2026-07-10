use std::net::SocketAddr;

use tokio::net::TcpListener;
use volo::net::DefaultIncoming;
use volo_http::{
    body::BodyConversion,
    client::Client,
    error::BoxError,
    server::{
        Redirect, Server,
        route::{Router, get, post},
    },
    utils::Extension,
};

#[derive(Clone)]
struct RedirectTarget {
    addr: SocketAddr,
}

async fn redirect_relative() -> Redirect {
    Redirect::found("/final")
}

async fn redirect_cross_host(Extension(target): Extension<RedirectTarget>) -> Redirect {
    Redirect::found(&format!("http://{}/landing", target.addr))
}

async fn redirect_post_to_get() -> Redirect {
    Redirect::see_other("/method")
}

async fn final_relative() -> &'static str {
    "followed relative redirect"
}

async fn landing() -> &'static str {
    "followed cross-host redirect"
}

async fn method() -> &'static str {
    "POST became GET after 303"
}

async fn bind_local() -> Result<(SocketAddr, DefaultIncoming), BoxError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    Ok((addr, DefaultIncoming::from(listener)))
}

#[volo::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .ok();

    let (target_addr, target_incoming) = bind_local().await?;
    let target_app = Router::new()
        .route("/landing", get(landing))
        .route("/method", get(method));
    let target_server = tokio::spawn(Server::new(target_app).run(target_incoming));

    let (entry_addr, entry_incoming) = bind_local().await?;
    let entry_app = Router::new()
        .route("/relative", get(redirect_relative))
        .route("/cross-host", get(redirect_cross_host))
        .route("/post-to-get", post(redirect_post_to_get))
        .route("/method", get(method))
        .route("/final", get(final_relative))
        .layer(Extension(RedirectTarget { addr: target_addr }));
    let entry_server = tokio::spawn(Server::new(entry_app).run(entry_incoming));

    let client = Client::builder().follow_redirects(10).build()?;

    let relative = client
        .get(format!("http://{entry_addr}/relative"))
        .send()
        .await?
        .into_string()
        .await?;
    let cross_host = client
        .get(format!("http://{entry_addr}/cross-host"))
        .send()
        .await?
        .into_string()
        .await?;
    let post_to_get = client
        .post(format!("http://{entry_addr}/post-to-get"))
        .data("payload that will be dropped")
        .send()
        .await?
        .into_string()
        .await?;

    assert_eq!(relative, "followed relative redirect");
    assert_eq!(cross_host, "followed cross-host redirect");
    assert_eq!(post_to_get, "POST became GET after 303");

    println!("relative: {relative}");
    println!("cross-host: {cross_host}");
    println!("post-to-get: {post_to_get}");

    entry_server.abort();
    target_server.abort();

    Ok(())
}
