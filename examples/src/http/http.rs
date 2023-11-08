use std::{convert::Infallible, net::SocketAddr};

use bytes::Bytes;
use http::{Method, Response, StatusCode, Uri};
use hyper::body::Incoming;
use motore::{service::service_fn, timeout::TimeoutLayer};
use serde::{Deserialize, Serialize};
use volo_http::{
    handler::HandlerService,
    request::Json,
    route::{Route, Router, ServiceLayerExt},
    server::Server,
    HttpContext,
};

async fn hello(
    _cx: &mut HttpContext,
    _request: Incoming,
) -> Result<Response<&'static str>, Infallible> {
    Ok(Response::new("hello, world\n"))
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
            Route::builder()
                .get(service_fn(hello))
                .build()
                .layer(TimeoutLayer::new(Some(std::time::Duration::from_secs(1)))),
        )
        .route("/:echo", Route::builder().get(service_fn(echo)).build())
        .route("/user", Route::builder().post(service_fn(json)).build())
        .route(
            "/test",
            Route::builder()
                .get(HandlerService::new(test))
                .post(HandlerService::new(test))
                .build(),
        )
        .layer(TimeoutLayer::new(Some(std::time::Duration::from_secs(1))));

    let addr: SocketAddr = "[::]:9091".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new(app).run(addr).await.unwrap();
}
