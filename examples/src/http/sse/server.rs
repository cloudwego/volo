use std::{convert::Infallible, net::SocketAddr};

use async_stream::stream;
use futures::Stream;
use tokio::time::Duration;
use volo::net::Address;
use volo_http::server::{
    Server,
    response::sse::{Event, KeepAlive, Sse},
    route::{Router, get},
};

async fn sse_handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = stream! {
        loop {
            yield Ok(Event::new().event("ping"));
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    };

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("do not kill me"),
    )
}

#[volo::main]
async fn main() {
    let app = Router::new().route("/sse", get(sse_handler));

    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let addr = Address::from(addr);

    println!("Server running on {}", addr);

    Server::new(app).run(addr).await.unwrap();
}
