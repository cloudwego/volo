use std::{net::SocketAddr, time::Duration};

use faststr::FastStr;
use serde::{Deserialize, Serialize};
use volo_http::{
    layer::TimeoutLayer,
    middleware::{self, Next},
    route::{get, post, MethodRouter, Router},
    Address, Bytes, ConnectionInfo, HttpContext, Incoming, Json, MaybeInvalid, Method, Params,
    Server, StatusCode, Uri,
};

async fn hello() -> &'static str {
    "hello, world\n"
}

#[derive(Serialize, Deserialize, Debug)]
struct Person {
    name: String,
    age: u8,
    phones: Vec<String>,
}

async fn json_get() -> Json<Person> {
    Json(Person {
        name: "Foo".to_string(),
        age: 25,
        phones: vec!["Bar".to_string(), "114514".to_string()],
    })
}

async fn json_post(Json(request): Json<Person>) -> String {
    let first_phone = request
        .phones
        .get(0)
        .map(|p| p.as_str())
        .unwrap_or("no number");
    format!(
        "{} is {} years old, {}'s first phone number is `{}`\n",
        request.name, request.age, request.name, first_phone
    )
}

async fn test(
    u: Uri,
    m: Method,
    data: MaybeInvalid<FastStr>,
) -> Result<String, (StatusCode, &'static str)> {
    let msg = unsafe { data.assume_valid() };
    match m {
        Method::GET => Err((StatusCode::BAD_REQUEST, "Try POST something\n")),
        Method::POST => Ok(format!("{m} {u}\n\n{msg}\n")),
        _ => unreachable!(),
    }
}

async fn conn_show(conn: ConnectionInfo) -> String {
    format!("{conn:?}\n")
}

async fn timeout_test() {
    tokio::time::sleep(Duration::from_secs(10)).await
}

async fn echo(params: Params) -> Result<Bytes, StatusCode> {
    if let Some(echo) = params.get("echo") {
        return Ok(echo.clone());
    }
    Err(StatusCode::BAD_REQUEST)
}

fn timeout_handler(uri: Uri, peer: Address) -> StatusCode {
    tracing::info!("Timeout on `{}`, peer: {}", uri, peer);
    StatusCode::INTERNAL_SERVER_ERROR
}

fn index_router() -> Router {
    // curl http://127.0.0.1:8080/
    Router::new().route("/", get(hello))
}

fn user_router() -> Router {
    Router::new()
        // curl http://localhost:8080/user/json_get
        .route("/user/json_get", get(json_get))
        // curl http://localhost:8080/user/json_post \
        //     -X POST \
        //     -H "Content-Type: application/json" \
        //     -d '{"name":"Foo", "age": 25, "phones":["Bar", "114514"]}'
        .route("/user/json_post", post(json_post))
}

fn test_router() -> Router {
    Router::new()
        // curl http://127.0.0.1:8080/test/extract
        // curl http://127.0.0.1:8080/test/extract -X POST -d "114514"
        .route(
            "/test/extract",
            MethodRouter::builder().get(test).post(test).build(),
        )
        // curl http://127.0.0.1:8080/test/timeout
        .route(
            "/test/timeout",
            get(timeout_test).layer(TimeoutLayer::new(Duration::from_secs(1), timeout_handler)),
        )
        // curl -v http://127.0.0.1:8080/test/param/114514
        .route("/test/param/:echo", get(echo))
        .route("/test/conn_show", get(conn_show))
}

async fn middleware_noarg_test(cx: &mut HttpContext, req: Incoming, next: Next) -> StatusCode {
    let _ = next.run(cx, req).await;
    StatusCode::OK
}

async fn middleware_arg_test(
    _uri: Uri,
    _cx: &mut HttpContext,
    _req: Incoming,
    _next: Next,
) -> StatusCode {
    StatusCode::NOT_FOUND
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let app = Router::new()
        .merge(index_router())
        .merge(user_router())
        .merge(test_router())
        .layer(middleware::from_fn(middleware_noarg_test))
        .layer(middleware::from_fn(middleware_arg_test))
        .layer(TimeoutLayer::new(Duration::from_secs(5), || {
            StatusCode::INTERNAL_SERVER_ERROR
        }));

    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    println!("Listening on {addr}");

    Server::new(app).run(addr).await.unwrap();
}
