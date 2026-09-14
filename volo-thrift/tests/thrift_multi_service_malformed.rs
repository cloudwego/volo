//! Security regression test: a malformed/truncated thrift binary frame sent to a
//! multi-service Router server must not abort the process (OOB read); the default
//! safe codec returns a protocol error and the server keeps serving.

// Keep this test in volo-thrift so the gate follows the runtime's actual feature
// selection, including features enabled through other workspace dependencies.
#![cfg(not(feature = "unsafe-codec"))]

use std::{net::SocketAddr, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
};
use volo_thrift::server::{Router, Server};

#[derive(Clone)]
struct HelloServiceImpl;

impl volo_gen::thrift_gen::hello::HelloService for HelloServiceImpl {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        Ok(volo_gen::thrift_gen::hello::HelloResponse {
            message: format!("Hello, {}!", req.name).into(),
            _field_mask: None,
        })
    }
}

fn malformed_frame() -> Vec<u8> {
    // strict binary message: name "Hello", type Call(1), seqid 1
    let name = b"Hello";
    let mut inner = Vec::new();
    inner.extend_from_slice(&0x80010001u32.to_be_bytes()); // strict | CALL
    inner.extend_from_slice(&(name.len() as i32).to_be_bytes());
    inner.extend_from_slice(name);
    inner.extend_from_slice(&1i32.to_be_bytes()); // seqid
    // args struct: field type I64 (10), field id 1, then only 3 of 8 value bytes
    inner.push(0x0a);
    inner.extend_from_slice(&1i16.to_be_bytes());
    inner.extend_from_slice(&[0u8; 3]); // truncated i64 -> OOB read in unsafe decoder

    let mut frame = (inner.len() as i32).to_be_bytes().to_vec();
    frame.extend_from_slice(&inner);
    frame
}

#[tokio::test]
async fn malformed_frame_does_not_abort_router_server() {
    let (tx, rx) = oneshot::channel::<()>();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    tokio::time::sleep(Duration::from_millis(10)).await;

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let addr = volo::net::Address::from(addr);

    let hello_service =
        volo_gen::thrift_gen::hello::HelloServiceServer::from_handler(HelloServiceImpl);
    let router = Router::new().with_default_service(hello_service);

    tokio::spawn(async move {
        let server = Server::with_router(router);
        tokio::select! {
            r = server.run(addr) => { let _ = r; }
            _ = rx => {}
        }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // 1. send malformed frame: server must answer with an exception frame or close, but the process
    //    must stay alive
    let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    sock.write_all(&malformed_frame()).await.unwrap();
    let mut buf = vec![0u8; 64];
    // either an exception reply, an EOF, or just silence: all are fine as long
    // as the process keeps running
    let _ = tokio::time::timeout(Duration::from_secs(2), sock.read(&mut buf)).await;
    drop(sock);

    // 2. the server process is still alive: a normal request through the generated client must
    //    succeed
    let saddr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let client = volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(saddr)
        .build();
    let resp = client
        .hello(volo_gen::thrift_gen::hello::HelloRequest {
            name: "World".into(),
            hello: None,
            _field_mask: None,
        })
        .await
        .expect("server must still serve after malformed frame");
    assert_eq!(resp.message.as_str(), "Hello, World!");

    let _ = tx.send(());
}
