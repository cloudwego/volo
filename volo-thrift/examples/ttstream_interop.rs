//! Interoperability endpoint for the shared Kitex runner.
use std::{collections::HashMap, error::Error};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use volo_thrift::{
    streaming::{Metadata, RawStruct, StreamConfig, StreamError},
    transport::ttstream::connection::{Connection, StreamRouter},
};

fn encode(value: &str, seq: i32) -> Bytes {
    let mut bytes = BytesMut::new();
    bytes.put_u8(11);
    bytes.put_i16(1);
    bytes.put_i32(value.len() as i32);
    bytes.extend_from_slice(value.as_bytes());
    bytes.put_u8(8);
    bytes.put_i16(2);
    bytes.put_i32(seq);
    bytes.put_u8(0);
    bytes.freeze()
}
fn decode(mut bytes: Bytes) -> Result<(String, i32), StreamError> {
    let mut value = String::new();
    let mut seq = 0;
    while !bytes.is_empty() {
        let ty = bytes.get_u8();
        if ty == 0 {
            break;
        }
        if bytes.len() < 2 {
            return Err(StreamError::Protocol("short field".into()));
        }
        let id = bytes.get_i16();
        match (ty, id) {
            (11, 1) => {
                if bytes.len() < 4 {
                    return Err(StreamError::Protocol("short string".into()));
                }
                let len = bytes.get_i32();
                if len < 0 || len as usize > bytes.len() {
                    return Err(StreamError::Protocol("bad string length".into()));
                }
                value = String::from_utf8(bytes.split_to(len as usize).to_vec())
                    .map_err(|e| StreamError::Protocol(e.to_string()))?;
            }
            (8, 2) => {
                if bytes.len() < 4 {
                    return Err(StreamError::Protocol("short i32".into()));
                }
                seq = bytes.get_i32();
            }
            _ => return Err(StreamError::Protocol("unknown fixture field".into())),
        }
    }
    Ok((value, seq))
}
fn router(service: &str) -> StreamRouter {
    let mut router = StreamRouter::new();
    router
        .add(service, "Bidi", |_, stream| async move {
            let (mut tx, mut rx) = stream.into_raw_split();
            while let Some(bytes) = rx.recv_struct().await? {
                tx.send_struct(bytes).await?;
            }
            Ok(())
        })
        .unwrap();
    router
        .add(service, "Client", |_, stream| async move {
            let (mut tx, mut rx) = stream.into_raw_split();
            let mut count = 0;
            let mut last = String::new();
            while let Some(bytes) = rx.recv_struct().await? {
                last = decode(bytes.into_bytes())?.0;
                count += 1;
            }
            tx.send_struct(RawStruct::from_bytes(encode(&last, count))?)
                .await?;
            Ok(())
        })
        .unwrap();
    router
        .add(service, "Server", |_, stream| async move {
            let (mut tx, mut rx) = stream.into_raw_split();
            let bytes = rx.recv_struct().await?.ok_or_else(|| {
                StreamError::Application(pilota::thrift::ApplicationException::new(
                    pilota::thrift::ApplicationExceptionKind::INTERNAL_ERROR,
                    "EOF",
                ))
            })?;
            let (value, count) = decode(bytes.into_bytes())?;
            for index in 0..count {
                tx.send_struct(RawStruct::from_bytes(encode(&value, index))?)
                    .await?;
            }
            Ok(())
        })
        .unwrap();
    router
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = HashMap::new();
    let mut argv = std::env::args().skip(1);
    while let Some(key) = argv.next() {
        args.insert(key, argv.next().ok_or("missing option value")?);
    }
    if args.get("--pool").is_some_and(|pool| pool != "direct") {
        return Err("raw endpoint supports only --pool direct".into());
    }
    let role = args.get("--role").map(String::as_str).unwrap_or("server");
    let addr = args
        .get("--addr")
        .map(String::as_str)
        .unwrap_or("tcp://127.0.0.1:18090");
    let service = args
        .get("--service")
        .map(String::as_str)
        .unwrap_or("Interop");
    if role == "server" {
        let router = router(service);
        if let Some(addr) = addr.strip_prefix("tcp://") {
            let listener = tokio::net::TcpListener::bind(addr).await?;
            println!(
                "{{\"event\":\"ready\",\"api\":\"raw\",\"raw_struct_validated\":true,\"addr\":\"\
                 tcp://{}\"}}",
                listener.local_addr()?
            );
            loop {
                let (socket, _) = listener.accept().await?;
                let (reader, writer) = socket.into_split();
                Connection::start(
                    reader,
                    writer,
                    StreamConfig::default(),
                    None,
                    Some(router.clone()),
                )?;
            }
        } else if let Some(path) = addr.strip_prefix("unix://") {
            let listener = tokio::net::UnixListener::bind(path)?;
            println!(
                "{{\"event\":\"ready\",\"api\":\"raw\",\"raw_struct_validated\":true,\"addr\":{}}}",
                sonic_rs::to_string(addr)?
            );
            loop {
                let (socket, _) = listener.accept().await?;
                let (reader, writer) = socket.into_split();
                Connection::start(
                    reader,
                    writer,
                    StreamConfig::default(),
                    None,
                    Some(router.clone()),
                )?;
            }
        } else {
            return Err("unsupported address".into());
        }
    }
    let connection = if let Some(addr) = addr.strip_prefix("tcp://") {
        let (socket, writer) = tokio::net::TcpStream::connect(addr).await?.into_split();
        Connection::start(socket, writer, StreamConfig::default(), None, None)?
    } else if let Some(path) = addr.strip_prefix("unix://") {
        let (reader, writer) = tokio::net::UnixStream::connect(path).await?.into_split();
        Connection::start(reader, writer, StreamConfig::default(), None, None)?
    } else {
        return Err("unsupported address".into());
    };
    let mode = args.get("--mode").map(String::as_str).unwrap_or("bidi");
    let count = args
        .get("--messages")
        .map(|s| s.parse::<i32>())
        .transpose()?
        .unwrap_or(3);
    let payload = args.get("--payload").map(String::as_str).unwrap_or("hello");
    let method = args
        .get("--method")
        .map(String::as_str)
        .unwrap_or(match mode {
            "client" => "Client",
            "server" => "Server",
            _ => "Bidi",
        });
    let stream = connection
        .open(service, method, Metadata::default())
        .await?;
    let (mut tx, mut rx) = stream.into_raw_split();
    if mode == "server" {
        tx.send_struct(RawStruct::from_bytes(encode(payload, count))?)
            .await?;
    } else {
        for seq in 0..count {
            tx.send_struct(RawStruct::from_bytes(encode(payload, seq))?)
                .await?;
        }
    }
    tx.close_send().await?;
    let mut responses = Vec::new();
    while let Some(bytes) = rx.recv_struct().await? {
        let (value, seq) = decode(bytes.into_bytes())?;
        responses.push(format!(
            "{{\"value\":{},\"seq\":{seq}}}",
            sonic_rs::to_string(&value)?
        ));
    }
    let report = format!(
        "{{\"event\":\"result\",\"ok\":true,\"api\":\"raw\",\"raw_struct_validated\":true,\"pool\"\
         :\"direct\",\"mode\":{},\"responses\":[{}],\"status\":\"ok\"}}",
        sonic_rs::to_string(mode)?,
        responses.join(",")
    );
    println!("{report}");
    if let Some(path) = args.get("--report") {
        std::fs::write(path, report)?;
    }
    connection.close();
    Ok(())
}
