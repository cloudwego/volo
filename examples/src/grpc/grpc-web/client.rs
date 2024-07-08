use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::{
    header::{self, ACCEPT, CONTENT_TYPE},
    Method, Request, StatusCode, Uri,
};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioExecutor;
use pilota::{prost::Message, FastStr};
use volo_gen::proto_gen::helloworld::{HelloReply, HelloRequest};
use volo_http::body::BodyConversion;

#[volo::main]
async fn main() {
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();

    let req = build_request("http://127.0.0.1:8080", "grpc-web", "grpc-web");
    let res = client.request(req).await.unwrap();
    let (parts, body) = res.into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();

    assert_eq!(parts.status, StatusCode::OK);
    assert_eq!(content_type, "application/grpc-web+proto");

    let (message, trailers) = decode_body(body).await;
    let expected = HelloReply {
        message: FastStr::from_static_str("helloworld, Volo!"),
    };

    assert_eq!(message, expected);
    assert_eq!(&trailers[..], b"grpc-status:0\r\n");
}

fn build_request(base_uri: &str, content_type: &str, accept: &str) -> Request<Full<Bytes>> {
    let request_uri = format!("{}/{}/{}", base_uri, "helloworld.Greeter", "SayHello")
        .parse::<Uri>()
        .unwrap();

    let bytes = match content_type {
        "grpc-web" => encode_body(),
        _ => panic!("invalid content type {}", content_type),
    };

    Request::builder()
        .method(Method::POST)
        .header(CONTENT_TYPE, format!("application/{}", content_type))
        .header(ACCEPT, format!("application/{}", accept))
        .uri(request_uri)
        .body(Full::new(bytes))
        .unwrap()
}

fn encode_body() -> Bytes {
    let input = HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };

    let mut buf = BytesMut::with_capacity(1024);
    buf.reserve(5);
    unsafe {
        buf.advance_mut(5);
    }

    input.encode(&mut buf).unwrap();

    let len = buf.len() - 5;
    {
        let mut buf = &mut buf[..5];
        buf.put_u8(0);
        buf.put_u32(len as u32);
    }

    buf.split_to(len + 5).freeze()
}

async fn decode_body(body: Incoming) -> (HelloReply, Bytes) {
    let mut body = body.collect().await.unwrap().into_bytes().await.unwrap();

    body.advance(1);
    let len = body.get_u32();
    let msg = HelloReply::decode(&mut body.split_to(len as usize)).expect("decode");
    body.advance(5);

    (msg, body)
}
