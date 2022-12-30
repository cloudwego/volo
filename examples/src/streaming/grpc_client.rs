#![feature(type_alias_impl_trait)]

use std::net::SocketAddr;

use async_stream::stream;
use lazy_static::lazy_static;
use tokio_stream::StreamExt;

lazy_static! {
    static ref CLIENT: volo_gen::proto_gen::streaming::StreamingClient = {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::streaming::StreamingClientBuilder::new("streaming")
            .address(addr)
            .build()
    };
}

#[volo::main]
async fn main() {
    unary().await;
    client_streaming().await;
    server_streaming().await;
    bidirectional_streaming().await;
}

async fn unary() {
    let req = volo_gen::proto_gen::streaming::StreamingRequest {
        message: "Volo".to_string(),
    };
    match CLIENT.unary(req).await {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("Unary, {e:?}"),
    }
}

async fn client_streaming() {
    let req = volo_gen::proto_gen::streaming::StreamingRequest {
        message: "Volo".to_string(),
    };
    let stream_req = stream! {
        for _ in 0..10 {
            yield req.clone();
        }
    };
    match CLIENT.client_streaming(stream_req).await {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("ClientStreaming, {e:?}"),
    }
}

async fn server_streaming() {
    let req = volo_gen::proto_gen::streaming::StreamingRequest {
        message: "Volo".to_string(),
    };
    let stream_resp = match CLIENT.server_streaming(req).await {
        Ok(resp) => resp.into_inner(),
        Err(e) => {
            eprintln!("ServerStreaming, {e:?}");
            return;
        }
    };
    let mut stream_resp = stream_resp.take(10);
    loop {
        match stream_resp.next().await {
            Some(Ok(info)) => {
                println!("{info:?}")
            }
            Some(Err(e)) => {
                eprintln!("ServerStreaming, {e:?}");
            }
            None => {
                break;
            }
        }
    }
}

async fn bidirectional_streaming() {
    let req = volo_gen::proto_gen::streaming::StreamingRequest {
        message: "Volo".to_string(),
    };
    let stream_req = stream! {
        for _ in 0..10 {
            yield req.clone();
        }
    };
    let stream_resp = match CLIENT.bidirectional_streaming(stream_req).await {
        Ok(resp) => resp.into_inner(),
        Err(e) => {
            eprintln!("BidirectionalStreaming, {e:?}");
            return;
        }
    };
    let mut stream_resp = stream_resp.take(10);
    loop {
        match stream_resp.next().await {
            Some(Ok(info)) => {
                println!("{info:?}")
            }
            Some(Err(e)) => {
                eprintln!("BidirectionalStreaming, {e:?}");
            }
            None => {
                break;
            }
        }
    }
}
