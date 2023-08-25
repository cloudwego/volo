use std::{convert::Infallible, net::SocketAddr};

use bytes::Bytes;
<<<<<<< HEAD
use http::{Method, Response, StatusCode, Uri};
use hyper::body::Incoming;
use motore::{service::service_fn, timeout::TimeoutLayer};
use serde::{Deserialize, Serialize};
use volo_http::{
    handler::HandlerService,
    request::Json,
    route::{Route, Router, Server, ServiceLayerExt},
=======
use http::{Response, StatusCode};
use hyper::body::Incoming;
use motore::service::service_fn;
use serde::{Deserialize, Serialize};
use volo_http::{
    request::Json,
    route::{Route, Router},
>>>>>>> init
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

<<<<<<< HEAD
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
    Router::new()
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
        .layer(TimeoutLayer::new(Some(std::time::Duration::from_secs(1))))
=======
#[tokio::main(flavor = "multi_thread")]
async fn main() {
    Router::build()
        .route("/", Route::builder().get(service_fn(hello)).build())
        .route("/:echo", Route::builder().get(service_fn(echo)).build())
        .route("/user", Route::builder().post(service_fn(json)).build())
>>>>>>> init
        .serve(SocketAddr::from(([127, 0, 0, 1], 3000)))
        .await
        .unwrap();
}
