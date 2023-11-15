use std::net::SocketAddr;

use bytes::Bytes;
use http::{Method, StatusCode, Uri};
use motore::timeout::TimeoutLayer;
use serde::Deserialize;
use volo_http::{
    handler::HandlerService,
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
    Json(request): Json<Person>,
) -> Result<&'static str, (StatusCode, &'static str)> {
    println!("{u:?}");
    println!("{m:?}");
    println!("{request:?}");
    if u.to_string().ends_with("a") {
        Ok("a") // http://localhost:3000/test?a=a
    } else {
        Err((StatusCode::BAD_REQUEST, "b")) // http://localhost:3000/test?a=bb
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let app = Router::new()
        .route(
            "/",
            get(hello).layer(TimeoutLayer::new(Some(std::time::Duration::from_secs(1)))),
        )
        .route("/:echo", get(echo))
        .route("/user", post(json))
        .route(
            "/test",
            MethodRouter::builder()
                .get(HandlerService::new(test))
                .post(HandlerService::new(test))
                .build(),
        )
        .layer(TimeoutLayer::new(Some(std::time::Duration::from_secs(1))));

    let addr: SocketAddr = "[::]:9091".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new(app).run(addr).await.unwrap();
}
