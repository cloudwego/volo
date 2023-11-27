use std::{net::SocketAddr, sync::Arc, time::Duration};

use motore::timeout::TimeoutLayer;
use volo_http::{
    route::{get, Router},
    server::Server,
    State,
};

async fn hello() -> &'static str {
    "hello, world\n"
}

struct AppState {
    val: usize,
}

async fn state_test(State(state): State<Arc<AppState>>) -> Result<String, String> {
    Ok(format!("{}", state.val))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let app = Router::new()
        .route(
            "/",
            get(hello).layer(TimeoutLayer::new(Some(Duration::from_secs(1)))),
        )
        .route("/state", get(state_test))
        .layer(TimeoutLayer::new(Some(Duration::from_secs(1))))
        .with_state(Arc::new(AppState { val: 114514 }));

    let addr: SocketAddr = "[::]:9091".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    println!("Listening on {addr}");

    Server::new(app).run(addr).await.unwrap();
}
