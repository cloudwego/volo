use std::net::SocketAddr;

use http::header;
use serde::{Deserialize, Serialize};
use volo_http::{
    body::BodyConversion,
    client::{get, ClientBuilder},
    error::BoxError,
    Json,
};

#[derive(Deserialize, Serialize, Debug)]
struct Person {
    name: String,
    age: u8,
    phones: Vec<String>,
}

#[volo::main]
async fn main() -> Result<(), BoxError> {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // simple `get` function with dns resolve
    println!(
        "{}",
        get("http://httpbin.org/get").await?.into_string().await?
    );

    // HTTPS `get`
    #[cfg(feature = "__tls")]
    {
        println!(
            "{}",
            get("https://httpbin.org/get").await?.into_string().await?
        );
    }

    // create client by builder
    let client = {
        let mut builder = ClientBuilder::new();
        builder
            .caller_name("example.http.client")
            .callee_name("example.http.server")
            // set default target address
            .address("127.0.0.1:8080".parse::<SocketAddr>().unwrap())
            .header("Test", "Test")?
            .fail_on_error_status(true);
        builder.build()
    };

    // set host and override the default one
    println!(
        "{}",
        client
            .request_builder()
            .host("httpbin.org")
            .uri("/get")?
            .send()
            .await?
            .into_string()
            .await?
    );

    println!(
        "{}",
        client
            .get("http://127.0.0.1:8080/")?
            .send()
            .await?
            .into_string()
            .await?
    );

    // use default target address
    println!(
        "{:?}",
        client
            .request_builder()
            .uri("/user/json_get")?
            .send()
            .await?
            .into_json::<Person>()
            .await?
    );
    println!(
        "{:?}",
        client
            .post("/user/json_post")?
            // `Content-Type` is needed!
            //
            // Without `Content-Type`, server will response with 415 Unsupported Media Type
            .header(header::CONTENT_TYPE, "application/json")?
            .data(Json(Person {
                name: "Foo".to_string(),
                age: 25,
                phones: vec!["114514".to_string()],
            }))?
            .send()
            .await?
            .into_string()
            .await?
    );

    // an empty client
    let client = ClientBuilder::new().build();
    println!(
        "{}",
        client
            .get("http://127.0.0.1:8080/")?
            .send()
            .await?
            .into_string()
            .await?
    );

    // invalid request because there is no target address
    println!(
        "{:?}",
        client
            .get("/")?
            .send()
            .await
            .expect_err("this request should fail"),
    );

    Ok(())
}
