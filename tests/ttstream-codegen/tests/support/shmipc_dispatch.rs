//! Raw SDK exchange shared by codec-baseline fixtures.
//! SDK shutdown closes the logical stream in both directions, unlike TCP SHUT_WR.
//! Valid exchanges therefore read complete replies before closing the SDK stream.
use std::time::Duration;

use pilota::{
    Bytes,
    thrift::{TInputProtocol, TType, binary::TBinaryProtocol},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use volo::net::{
    Address,
    dial::{DefaultMakeTransport, MakeTransport},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub bytes: Vec<u8>,
    pub stream_closed: bool,
    pub closed_by_test_before_read: bool,
    pub read_end: Option<&'static str>,
}
fn complete_replies(mut bytes: &[u8]) -> usize {
    let mut count = 0;
    while bytes.len() >= 4 {
        if bytes[0] == 0x80 {
            let mut remaining = Bytes::copy_from_slice(bytes);
            let mut protocol = TBinaryProtocol::new(&mut remaining, false);
            if protocol.read_message_begin().is_err()
                || protocol.skip(TType::Struct).is_err()
                || protocol.read_message_end().is_err()
            {
                break;
            }
            let consumed = bytes.len() - remaining.len();
            bytes = &bytes[consumed..];
        } else {
            let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            if len > bytes.len() - 4 {
                break;
            }
            bytes = &bytes[4 + len..];
        }
        count += 1;
    }
    count
}
pub async fn exchange(
    address: Address,
    bytes: &[u8],
    fragment_prefix: bool,
    replies: usize,
    expect_peer_close: bool,
) -> Outcome {
    let (mut reader, mut writer) = DefaultMakeTransport::new()
        .make_transport(address)
        .await
        .unwrap();
    if fragment_prefix {
        for byte in &bytes[..8] {
            writer.write_all(&[*byte]).await.unwrap();
            writer.flush().await.unwrap();
            tokio::task::yield_now().await;
        }
        writer.write_all(&bytes[8..]).await.unwrap();
    } else {
        writer.write_all(bytes).await.unwrap();
    }
    writer.flush().await.unwrap();
    let local_eof = bytes.len() < 8;
    if local_eof {
        writer.shutdown().await.unwrap();
    }
    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
        let mut output = Vec::new();
        let mut read_end = None;
        loop {
            let mut chunk = [0u8; 4096];
            let size = match reader.read(&mut chunk).await {
                Ok(0) => {
                    read_end = Some("eof");
                    break;
                }
                Ok(size) => size,
                Err(error)
                    if error.kind() == std::io::ErrorKind::ConnectionAborted
                        && matches!(
                            error
                                .get_ref()
                                .and_then(|source| source.downcast_ref::<shmipc::Error>()),
                            Some(shmipc::Error::StreamClosed)
                        ) =>
                {
                    read_end = Some("sdk-stream-closed");
                    break;
                }
                Err(error) => panic!("unexpected SDK read error: {error:?}"),
            };
            output.extend_from_slice(&chunk[..size]);
            if !expect_peer_close && complete_replies(&output) >= replies {
                break;
            }
        }
        Outcome {
            bytes: output,
            stream_closed: read_end.is_some(),
            closed_by_test_before_read: local_eof,
            read_end,
        }
    })
    .await
    .expect("SDK exchange must finish within its declared reply/close boundary");
    writer.shutdown().await.unwrap();
    outcome
}
