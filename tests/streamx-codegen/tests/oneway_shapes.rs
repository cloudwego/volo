use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use pilota::thrift::{
    Message, TAsyncBinaryProtocol, TAsyncInputProtocol, TMessageIdentifier, TMessageType,
    TOutputProtocol, binary::TBinaryProtocol,
};
use streamx_codegen::oneway::oneway_shapes::*;
use tokio::io::{AsyncWriteExt, BufReader};
use volo::net::incoming::DefaultIncoming;
use volo_streamx::{RpcError, RpcResult, StreamContext};

#[derive(Default)]
struct Handler {
    calls: Mutex<Vec<(&'static str, Vec<i64>)>>,
    fail: AtomicBool,
}

impl Handler {
    fn record(&self, method: &'static str, args: Vec<i64>, cx: &StreamContext) -> RpcResult<()> {
        assert_eq!((cx.service(), cx.method()), ("Shapes", method));
        self.calls.lock().unwrap().push((method, args));
        if self.fail.load(Ordering::Relaxed) {
            Err(RpcError::Biz {
                code: 17,
                message: "oneway".into(),
                extra: Default::default(),
            })
        } else {
            Ok(())
        }
    }
}

impl shapes_streamx::Handler for Handler {
    async fn zero(&self, cx: StreamContext, _: ()) -> RpcResult<()> {
        self.record("Zero", vec![], &cx)
    }
    async fn one(&self, cx: StreamContext, value: i64) -> RpcResult<()> {
        self.record("One", vec![value], &cx)
    }
    async fn many(&self, cx: StreamContext, args: ShapesManyArgsRecv) -> RpcResult<()> {
        self.record("Many", vec![i64::from(args.left), args.right], &cx)
    }
    async fn count(&self, _: StreamContext, _: ()) -> RpcResult<i64> {
        Ok(self.calls.lock().unwrap().len() as i64)
    }
}

#[test]
fn all_oneway_shapes_remain_in_the_directory_with_only_pingpong_bindings() {
    let mut registry = volo_streamx::Registry::new();
    registry
        .register(shapes_streamx::bind(Handler::default()))
        .unwrap();
    let registry = registry.freeze().unwrap();
    for method in ["Zero", "One", "Many"] {
        let binding = registry.find_thrift("Shapes", method).unwrap();
        assert!(binding.method().oneway);
        assert!(
            volo_thrift::server::routed::UnaryLookup::find(&*registry, "Shapes", method).is_some()
        );
        assert!(
            volo_thrift::transport::ttstream::router::StreamLookup::find(
                &*registry, "Shapes", method
            )
            .is_none()
        );
        assert!(
            volo_grpc::server::RouteLookup::find(
                &*registry,
                &format!("/oneway_shapes.Shapes/{method}")
            )
            .is_err()
        );
    }
    assert!(
        volo_grpc::server::RouteLookup::find(&*registry, "/oneway_shapes.Shapes/Count").is_ok()
    );
}

fn request(method: &str, sequence: i32, oneway: bool, args: &impl Message) -> Vec<u8> {
    let mut bytes = pilota::LinkedBytes::new();
    let mut protocol = TBinaryProtocol::new(&mut bytes, true);
    protocol
        .write_message_begin(&TMessageIdentifier::new(
            method.to_owned().into(),
            if oneway {
                TMessageType::OneWay
            } else {
                TMessageType::Call
            },
            sequence,
        ))
        .unwrap();
    args.encode(&mut protocol).unwrap();
    protocol.write_message_end().unwrap();
    bytes.into_bytes_mut().to_vec()
}

#[tokio::test]
async fn successful_and_failed_oneway_shapes_never_reply_before_the_next_unary() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let shared = Arc::new(Handler::default());
        let prepared = volo_streamx::Server::new()
            .add_service(shapes_streamx::BoundService::from_arc(shared.clone()))
            .graceful_shutdown_timeout(Duration::from_millis(500))
            .prepare()
            .unwrap();
        let handle = prepared.handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
        let mut io = BufReader::new(tokio::net::TcpStream::connect(address).await.unwrap());
        for (round, fail) in [false, true].into_iter().enumerate() {
            shared.fail.store(fail, Ordering::Relaxed);
            let sequence = 20 + round as i32;
            let bytes = [
                request("Zero", 1, true, &ShapesZeroArgsSend {}),
                request("One", 2, true, &ShapesOneArgsSend { value: -3 }),
                request(
                    "Many",
                    3,
                    true,
                    &ShapesManyArgsSend {
                        left: 7,
                        right: 4_294_967_311,
                    },
                ),
                request("Count", sequence, false, &ShapesCountArgsSend {}),
            ]
            .concat();
            // Buffered is serial: Count proves all three handlers completed.
            // Any oneway success/error reply would appear before Count here.
            io.get_mut().write_all(&bytes).await.unwrap();
            let mut protocol = TAsyncBinaryProtocol::new(&mut io);
            let identifier = protocol.read_message_begin().await.unwrap();
            assert_eq!(identifier.name, "Count");
            assert_eq!(identifier.sequence_number, sequence);
            assert_eq!(identifier.message_type, TMessageType::Reply);
            let ShapesCountResultRecv::Ok(count) =
                ShapesCountResultRecv::decode_async(&mut protocol)
                    .await
                    .unwrap();
            assert_eq!(count, 3 * (round as i64 + 1));
            protocol.read_message_end().await.unwrap();
        }
        assert_eq!(
            *shared.calls.lock().unwrap(),
            [
                ("Zero", vec![]),
                ("One", vec![-3]),
                ("Many", vec![7, 4_294_967_311]),
                ("Zero", vec![]),
                ("One", vec![-3]),
                ("Many", vec![7, 4_294_967_311]),
            ]
        );
        drop(io);
        handle.shutdown();
        server.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
