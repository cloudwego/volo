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

    // simple `get` function and dns resolve
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
    let client = ClientBuilder::new()
        .caller_name("example.http.client")
        .callee_name("example.http.server")
        .header("Test", "Test")?
        .build();

    println!(
        "{}",
        client
            .get("http://127.0.0.1:8080/")?
            .send()
            .await?
            .into_string()
            .await?
    );
    println!(
        "{:?}",
        client
            .get("http://127.0.0.1:8080/user/json_get")?
            .send()
            .await?
            .into_json::<Person>()
            .await?
    );
    println!(
        "{:?}",
        client
            .post("http://127.0.0.1:8080/user/json_post")?
            // Content-Type is needed!
            .header("Content-Type", "application/json")?
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
    Ok(())
}
