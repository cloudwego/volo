use std::{convert::Infallible, net::SocketAddr};

use bytes::Bytes;
<<<<<<< HEAD
<<<<<<< HEAD
use http::{Method, Response, StatusCode, Uri};
use hyper::body::Incoming;
use motore::{service::service_fn, timeout::TimeoutLayer};
<<<<<<< HEAD
use serde::{Deserialize, Serialize};
use volo_http::{
    handler::HandlerService,
    request::Json,
<<<<<<< HEAD
    route::{Route, Router, Server, ServiceLayerExt},
=======
use http::{Response, StatusCode};
=======
use http::{Method, Response, StatusCode, Uri};
>>>>>>> handler, extractor (#221)
use hyper::body::Incoming;
use motore::service::service_fn;
=======
>>>>>>> layer (#224)
use serde::{Deserialize, Serialize};
use volo_http::{
    handler::HandlerService,
    request::Json,
<<<<<<< HEAD
    route::{Route, Router},
>>>>>>> init
=======
    route::{Route, Router, Server, ServiceLayerExt},
>>>>>>> layer (#224)
=======
    route::{Route, Router, ServiceLayerExt},
    server::Server,
>>>>>>> add graceful shutdown
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
<<<<<<< HEAD
=======
>>>>>>> handler, extractor (#221)
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

<<<<<<< HEAD
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
=======
>>>>>>> handler, extractor (#221)
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
<<<<<<< HEAD
>>>>>>> init
=======
        .route(
            "/test",
            Route::builder()
                .get(HandlerService::new(test))
                .post(HandlerService::new(test))
                .build(),
        )
<<<<<<< HEAD
<<<<<<< HEAD
>>>>>>> handler, extractor (#221)
=======
        .layer(TimeoutLayer::new(Some(std::time::Duration::from_secs(1))))
>>>>>>> layer (#224)
        .serve(SocketAddr::from(([127, 0, 0, 1], 3000)))
        .await
        .unwrap();
=======
        .layer(TimeoutLayer::new(Some(std::time::Duration::from_secs(1))));

    let addr: SocketAddr = "[::]:9091".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new(app).run(addr).await.unwrap();
>>>>>>> add graceful shutdown
}
