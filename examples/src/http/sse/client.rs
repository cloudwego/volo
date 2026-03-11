use volo_http::{
    client::{Client, sse::SseExt},
    error::BoxError,
};

#[volo::main]
async fn main() -> Result<(), BoxError> {
    let client = Client::builder().build()?;

    let mut reader = client
        .get("http://127.0.0.1:8080/sse")
        .send()
        .await?
        .into_sse()?;

    while let Some(event) = reader.read().await? {
        println!("event: {}", event.event());
        if let Some(data) = event.data() {
            println!("data: {}", data);
        }
        if let Some(id) = event.id() {
            println!("id: {}", id);
        }
        if let Some(retry) = event.retry() {
            println!("retry: {}", retry.as_millis());
        }
        println!();
    }

    Ok(())
}
