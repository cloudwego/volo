use std::{convert::Infallible, net::SocketAddr};

use bytes::Bytes;
use http::{Response, StatusCode};
use hyper::body::Incoming;
use motore::service::service_fn;
use serde::{Deserialize, Serialize};
use volo_http::{
    request::Json,
    route::{Route, Router},
    HttpContext,
};

async fn hello(
    _cx: &mut HttpContext,
    _request: Incoming,
) -> Result<Response<&'static str>, Infallible> {
    Ok(Response::new("hello, world"))
}

async fn echo(cx: &mut HttpContext, _request: Incoming) -> Result<Response<Bytes>, Infallible> {
    if let Some(echo) = cx.params.get("echo") {
        return Ok(Response::new(echo.clone()));
    }
    Ok(Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .body(Bytes::new())
        .unwrap())
}

#[derive(Serialize, Deserialize, Debug)]
struct Person {
    name: String,
    age: u8,
    phones: Vec<String>,
}

async fn json(
    _cx: &mut HttpContext,
    Json(request): Json<Person>,
) -> Result<Response<()>, Infallible> {
    let first_phone = request
        .phones
        .get(0)
        .map(|p| p.as_str())
        .unwrap_or("no number");
    println!(
        "{} is {} years old, {}'s first phone number is {}",
        request.name, request.age, request.name, first_phone
    );
    Ok(Response::new(()))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    Router::build()
        .route("/", Route::builder().get(service_fn(hello)).build())
        .route("/:echo", Route::builder().get(service_fn(echo)).build())
        .route("/user", Route::builder().post(service_fn(json)).build())
        .serve(SocketAddr::from(([127, 0, 0, 1], 3000)))
        .await
        .unwrap();
}
