#![cfg(feature = "ttstream")]
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use pilota::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ttstream_codegen::volo_gen::interop::{Interop, InteropClientBuilder, InteropServer, Message};
use volo::net::{Address, incoming::DefaultIncoming};
use volo_thrift::{
    streaming::{
        RequestStream, ResponseSender, StreamConfig, StreamContext, StreamError, encode_message,
    },
    transport::ttstream::{
        connection::StreamRouter,
        frame::{Frame, FrameType},
    },
};

struct Echo(Arc<AtomicUsize>);
impl Interop for Echo {
    async fn unary(&self, request: Message) -> Result<Message, volo_thrift::ServerError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(request)
    }
    async fn explicit_unary(&self, request: Message) -> Result<Message, volo_thrift::ServerError> {
        self.unary(request).await
    }
    async fn client(
        &self,
        _: StreamContext,
        mut input: RequestStream<Message>,
    ) -> Result<Message, StreamError> {
        Ok(input.recv().await?.unwrap_or_default())
    }
    async fn server(
        &self,
        _: StreamContext,
        request: Message,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        output.send(&request).await
    }
    async fn bidi(
        &self,
        _: StreamContext,
        mut input: RequestStream<Message>,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        while let Some(request) = input.recv().await? {
            output.send(&request).await?;
        }
        Ok(())
    }
}
trait Socket: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Socket for T {}
async fn connect(address: &Address) -> Box<dyn Socket> {
    match address {
        Address::Ip(address) => Box::new(tokio::net::TcpStream::connect(address).await.unwrap()),
        Address::Unix(address) => Box::new(
            tokio::net::UnixStream::connect(address.as_pathname().unwrap())
                .await
                .unwrap(),
        ),
        #[cfg(feature = "shmipc")]
        _ => panic!("fixture transport unsupported"),
    }
}
async fn bind(uds: bool, path: &std::path::Path) -> (Address, DefaultIncoming) {
    if uds {
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        (
            Address::Unix(std::os::unix::net::SocketAddr::from_pathname(path).unwrap()),
            listener.into(),
        )
    } else {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        (listener.local_addr().unwrap().into(), listener.into())
    }
}
async fn capture_real_unary_frame() -> Vec<u8> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = Address::from(listener.local_addr().unwrap());
    let client = tokio::spawn(async move {
        let _ = InteropClientBuilder::new("Interop")
            .address(address)
            .build()
            .unary(Message {
                value: "dispatch-fixture".into(),
                seq: 7,
            })
            .await;
    });
    let (mut connection, _) = listener.accept().await.unwrap();
    let length = connection.read_u32().await.unwrap();
    let mut frame = length.to_be_bytes().to_vec();
    frame.resize(length as usize + 4, 0);
    connection.read_exact(&mut frame[4..]).await.unwrap();
    client.abort();
    let _ = client.await;
    assert_eq!(&frame[4..6], &[0x10, 0]);
    assert_eq!(u16::from_be_bytes([frame[6], frame[7]]) & 2, 0);
    frame
}
fn unary_payload(frame: &[u8]) -> &[u8] {
    let header = usize::from(u16::from_be_bytes([frame[12], frame[13]])) * 4;
    &frame[14 + header..]
}
fn normalize_response(mut response: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while response.len() >= 4 {
        let length = u32::from_be_bytes(response[..4].try_into().unwrap()) as usize;
        if length > response.len() - 4 {
            break;
        }
        let frame = &response[..length + 4];
        if frame.len() >= 14 && frame[4..6] == [0x10, 0] {
            out.push(unary_payload(frame).to_vec());
        } else {
            out.push(frame[4..].to_vec());
        }
        response = &response[length + 4..];
    }
    if !response.is_empty() {
        out.push(response.to_vec());
    }
    out
}
async fn exchange(address: &Address, bytes: &[u8], fragment_prefix: bool) -> Vec<u8> {
    let mut socket = connect(address).await;
    if fragment_prefix {
        for byte in &bytes[..8] {
            socket.write_all(&[*byte]).await.unwrap();
            socket.flush().await.unwrap();
            tokio::task::yield_now().await;
        }
        socket.write_all(&bytes[8..]).await.unwrap();
    } else {
        socket.write_all(bytes).await.unwrap();
    }
    socket.shutdown().await.unwrap();
    let mut output = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut output))
        .await
        .expect("server must close after input EOF")
        .unwrap();
    output
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_dispatch_preserves_unary_codec_baseline_and_protocol_switch_errors() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let unary = capture_real_unary_frame().await;
        let payload = unary_payload(&unary);
        assert_eq!(
            u32::from_be_bytes(payload[..4].try_into().unwrap()) as usize,
            payload.len() - 4
        );
        let framed = payload.to_vec();
        let mut glued = unary.clone();
        glued.extend_from_slice(&unary);
        let config = StreamConfig::default();
        let header = Frame::new(
            11,
            FrameType::Header,
            Bytes::from_static(b"Bidi"),
            Bytes::new(),
        )
        .encode(&config)
        .unwrap();
        let mut switched = unary.clone();
        switched.extend_from_slice(&header);
        let cases = [
            ("ttheader", unary.clone(), false, 1),
            ("first-eight-fragments", unary.clone(), true, 1),
            ("two-glued", glued, false, 2),
            ("framed", framed, false, 1),
            ("buffered", payload[4..].to_vec(), false, 1),
            (
                "http2-baseline",
                b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec(),
                false,
                0,
            ),
            ("short-eof", unary[..4].to_vec(), false, 0),
            ("empty-eof", Vec::new(), false, 0),
            ("unary-then-streaming", switched, false, 1),
        ];
        for uds in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut outcomes = Vec::new();
            for dispatch in [false, true] {
                let (address, incoming) = bind(
                    uds,
                    &directory.path().join(format!("server-{dispatch}.sock")),
                )
                .await;
                let called = Arc::new(AtomicUsize::new(0));
                let builder = InteropServer::new(Echo(called.clone()));
                let builder = if dispatch {
                    builder
                } else {
                    builder.streaming_router(StreamRouter::new())
                };
                let server = tokio::spawn(builder.run(incoming));
                let mut responses = Vec::new();
                for (name, bytes, fragment, count) in &cases {
                    let before = called.load(Ordering::Relaxed);
                    let response = exchange(&address, bytes, *fragment).await;
                    assert_eq!(
                        called.load(Ordering::Relaxed) - before,
                        *count,
                        "uds={uds}, dispatch={dispatch}, case={name}"
                    );
                    responses.push(normalize_response(&response));
                }
                if dispatch {
                    let before = called.load(Ordering::Relaxed);
                    let mut input = header.to_vec();
                    let message = encode_message(&Message {
                        value: "stream".into(),
                        seq: 1,
                    })
                    .unwrap();
                    input.extend_from_slice(
                        &Frame::new(11, FrameType::Data, Bytes::from_static(b"Bidi"), message)
                            .encode(&config)
                            .unwrap(),
                    );
                    input.extend_from_slice(&unary);
                    let _ = exchange(&address, &input, false).await;
                    assert_eq!(
                        called.load(Ordering::Relaxed),
                        before,
                        "TTStream connection must not dispatch later unary frame"
                    );
                }
                server.abort();
                let _ = server.await;
                outcomes.push(responses);
            }
            assert_eq!(
                outcomes[0], outcomes[1],
                "nonmatching input must retain original unary response semantics, uds={uds}"
            );
        }
    })
    .await
    .unwrap();
}

#[cfg(feature = "shmipc")]
#[path = "support/shmipc_dispatch.rs"]
mod shmipc_dispatch;

#[cfg(feature = "shmipc")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_memory_dispatch_preserves_original_codec_reader_and_close_behavior() {
    use volo::net::shmipc::{
        self,
        config::{
            Config, MemMapType, SESSION_MANAGER_CONFIG, SessionManagerConfig, SizePercentPair,
        },
    };
    tokio::time::timeout(Duration::from_secs(45), async {
        let unary = capture_real_unary_frame().await;
        let payload = unary_payload(&unary);
        let config = StreamConfig::default();
        let header = Frame::new(
            11,
            FrameType::Header,
            Bytes::from_static(b"Bidi"),
            Bytes::new(),
        )
        .encode(&config)
        .unwrap();
        let mut glued = unary.clone();
        glued.extend_from_slice(&unary);
        let mut switched = unary.clone();
        switched.extend_from_slice(&header);
        let cases = [
            ("ttheader", unary.clone(), false, 1, false),
            ("first-eight-fragments", unary.clone(), true, 1, false),
            ("two-glued", glued, false, 2, false),
            ("framed", payload.to_vec(), false, 1, false),
            ("buffered", payload[4..].to_vec(), false, 1, false),
            (
                "http2-baseline",
                b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec(),
                false,
                0,
                true,
            ),
            ("short-eof", unary[..4].to_vec(), false, 0, true),
            ("empty-eof", Vec::new(), false, 0, true),
            ("unary-then-streaming", switched, false, 1, true),
        ];
        let mut outcomes = Vec::new();
        for dispatch in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let socket = std::os::unix::net::SocketAddr::from_pathname(
                directory.path().join("control.sock"),
            )
            .unwrap();
            let sdk = Config {
                queue_path: directory
                    .path()
                    .join("queue")
                    .to_string_lossy()
                    .into_owned(),
                share_memory_path_prefix: directory
                    .path()
                    .join("buffer")
                    .to_string_lossy()
                    .into_owned(),
                share_memory_buffer_cap: 8 << 20,
                buffer_slice_sizes: vec![SizePercentPair {
                    size: 4096,
                    percent: 100,
                }],
                mem_map_type: MemMapType::MemMapTypeDevShmFile,
                ..Default::default()
            };
            let listener =
                shmipc::Listener::listen(shmipc::Address::Unix(socket.clone()), Some(sdk.clone()))
                    .await
                    .unwrap();
            let called = Arc::new(AtomicUsize::new(0));
            let builder = InteropServer::new(Echo(called.clone()));
            let builder = if dispatch {
                builder
            } else {
                builder.streaming_router(StreamRouter::new())
            };
            let server = tokio::spawn(builder.run(DefaultIncoming::from(listener)));
            let address = Address::Shmipc(shmipc::Address::Unix(socket));
            let responses = SESSION_MANAGER_CONFIG
                .scope(
                    SessionManagerConfig::default()
                        .with_config(sdk)
                        .with_session_num(1),
                    async {
                        let mut responses = Vec::new();
                        for (name, bytes, fragment, count, close) in &cases {
                            let before = called.load(Ordering::Relaxed);
                            let result = shmipc_dispatch::exchange(
                                address.clone(),
                                bytes,
                                *fragment,
                                *count,
                                *close,
                            )
                            .await;
                            assert_eq!(
                                called.load(Ordering::Relaxed) - before,
                                *count,
                                "shmipc dispatch={dispatch}, case={name}"
                            );
                            assert_eq!(
                                result.stream_closed, *close,
                                "shmipc close boundary {name}"
                            );
                            responses.push((
                                normalize_response(&result.bytes),
                                result.stream_closed,
                                result.closed_by_test_before_read,
                                result.read_end,
                            ));
                        }
                        if dispatch {
                            let before = called.load(Ordering::Relaxed);
                            let mut input = header.to_vec();
                            input.extend_from_slice(
                                &Frame::new(
                                    11,
                                    FrameType::Data,
                                    Bytes::from_static(b"Bidi"),
                                    encode_message(&Message {
                                        value: "stream".into(),
                                        seq: 1,
                                    })
                                    .unwrap(),
                                )
                                .encode(&config)
                                .unwrap(),
                            );
                            input.extend_from_slice(&unary);
                            let result =
                                shmipc_dispatch::exchange(address, &input, false, 0, true).await;
                            assert!(result.stream_closed);
                            assert_eq!(
                                called.load(Ordering::Relaxed),
                                before,
                                "TTStream driver must reject later unary on SDK logical connection"
                            );
                        }
                        responses
                    },
                )
                .await;
            server.abort();
            let _ = server.await;
            outcomes.push(responses);
        }
        assert_eq!(
            outcomes[0], outcomes[1],
            "SDK baseline and automatic dispatch must preserve replies and close boundaries"
        );
    })
    .await
    .unwrap();
}

#[cfg(feature = "multiplex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pure_and_mixed_multiplex_servers_preserve_unary_and_streaming_calls() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for uds in [false, true] {
            for mixed in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let (address, incoming) = bind(uds, &directory.path().join("mux.sock")).await;
                let called = Arc::new(AtomicUsize::new(0));
                let builder = InteropServer::new(Echo(called.clone())).multiplex(true);
                let builder = if mixed {
                    builder
                } else {
                    builder.streaming_router(StreamRouter::new())
                };
                assert_eq!(builder.has_streaming_methods(), mixed);
                let server = tokio::spawn(builder.run(incoming));
                let client = InteropClientBuilder::new("Interop")
                    .address(address)
                    .multiplex(true)
                    .build();
                let request = Message {
                    value: "multiplex-unary-streaming".into(),
                    seq: 41,
                };
                assert_eq!(client.unary(request.clone()).await.unwrap(), request);
                if mixed {
                    let (mut tx, mut rx) = client.bidi().await.unwrap().into_split();
                    tx.send(&request).await.unwrap();
                    tx.close_send().await.unwrap();
                    assert_eq!(rx.recv().await.unwrap(), Some(request.clone()));
                    assert!(rx.recv().await.unwrap().is_none());
                }
                assert_eq!(
                    client.explicit_unary(request.clone()).await.unwrap(),
                    request
                );
                assert_eq!(called.load(Ordering::Relaxed), 2);
                drop(client);
                server.abort();
                let _ = server.await;
            }
        }
    })
    .await
    .unwrap();
}
