use std::{net::SocketAddr, time::Duration};

use bytes::Bytes;
use http::{Method, StatusCode, Uri};
use motore::timeout::TimeoutLayer;
use serde::Deserialize;
use volo_http::{
    param::Params,
    request::Json,
    route::{get, post, MethodRouter, Router},
    server::Server,
};

async fn hello() -> &'static str {
    "hello, world\n"
}

async fn echo(params: Params) -> Result<Bytes, StatusCode> {
    if let Some(echo) = params.get("echo") {
        return Ok(echo.clone());
    }
    Err(StatusCode::BAD_REQUEST)
}

#[derive(Deserialize, Debug)]
struct Person {
    name: String,
    age: u8,
    phones: Vec<String>,
}

async fn json(Json(request): Json<Person>) {
    let first_phone = request
        .phones
        .get(0)
        .map(|p| p.as_str())
        .unwrap_or("no number");
    println!(
        "{} is {} years old, {}'s first phone number is {}",
        request.name, request.age, request.name, first_phone
    );
}

async fn test(
    u: Uri,
    m: Method,
) -> Result<&'static str, (StatusCode, &'static str)> {
    println!("uri:    {u:?}");
    println!("method: {m:?}");
    if u.to_string().ends_with("a") {
        Ok("a") // http://localhost:3000/test?a=a
    } else {
        Err((StatusCode::BAD_REQUEST, "b")) // http://localhost:3000/test?a=bb
    }
}

async fn timeout_test() {
    tokio::time::sleep(Duration::from_secs(5)).await
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let app = Router::new()
        .route(
            "/",
            get(hello).layer(TimeoutLayer::new(Some(Duration::from_secs(1)))),
        )
        .route("/:echo", get(echo))
        .route("/user", post(json))
        .route(
            "/test",
            MethodRouter::builder().get(test).post(test).build(),
        )
        .route("/timeout", get(timeout_test))
        .layer(TimeoutLayer::new(Some(Duration::from_secs(1))));

    let addr: SocketAddr = "[::]:9091".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    println!("Listening on {addr}");

    Server::new(app).run(addr).await.unwrap();
}
