use volo_http::{
    client::{Client, sse::SseReader},
    error::BoxError,
};

#[volo::main]
async fn main() -> Result<(), BoxError> {
    let client = Client::builder().build()?;

    let resp = client.get("http://127.0.0.1:8080/sse").send().await?;

    let mut reader = SseReader::new(resp)?;

    while let Some(sse_event) = reader.read().await? {
        println!("Event: {:?}", sse_event.event());
        println!("Data: {:?}", sse_event.data());
        println!("ID: {:?}", sse_event.id());
        println!("Retry: {:?}", sse_event.retry());
        println!("Comment: {:?}\n", sse_event.comment());
    }

    Ok(())
}
