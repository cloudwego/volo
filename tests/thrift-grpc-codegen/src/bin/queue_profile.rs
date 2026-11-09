//! Measure a prepared channel's queued bytes/messages and bounded wait/recovery.
use std::time::{Duration, Instant};

use futures::StreamExt;
use pilota::thrift::{Message as _, binary::TBinaryProtocol};
use thrift_grpc_codegen::volo_gen::grpc_parent::Message;
use volo_grpc::{
    Status,
    message::CodecOptions,
    thrift::{ChannelConfig, channel_with_config},
};

async fn measure(repetition: usize, byte_limited: bool) -> Result<sonic_rs::Value, Status> {
    let message = Message {
        payload: Some(pilota::Bytes::from(vec![b'x'; 1024])),
        sequence: Some(1),
    };
    let encoded = message.size(&mut TBinaryProtocol::new((), true));
    let config = ChannelConfig {
        capacity: if byte_limited { 4 } else { 1 },
        max_buffered_bytes: encoded * if byte_limited { 2 } else { 4 },
        max_message_size: encoded,
    };
    let (mut sender, source) = channel_with_config(config)?;
    let high_water = if byte_limited { 2 } else { 1 };
    for _ in 0..high_water {
        sender.send(&message).await?;
    }
    let mut body = source.into_body(None, CodecOptions::new(Default::default()), None);
    let (first, waited) = {
        let next = sender.send(&message);
        futures::pin_mut!(next);
        if !futures::poll!(&mut next).is_pending() {
            return Err(Status::internal("full channel did not apply backpressure"));
        }
        // The receiver deliberately stays idle. This is queue wait, not a claim
        // about network flow control (which has a separate live H2 fixture).
        let waiting = Instant::now();
        std::thread::sleep(Duration::from_millis(5));
        let first = body.next().await.unwrap()?;
        next.await?;
        (first, waiting.elapsed().as_micros() as u64)
    };
    drop(sender);
    let mut bytes = first.data_ref().map_or(0, |value| value.len());
    drop(first);
    while let Some(frame) = body.next().await {
        bytes += frame?.data_ref().map_or(0, |value| value.len());
    }
    if bytes != (high_water + 1) * (encoded + 5) || waited < 5_000 {
        return Err(Status::internal(
            "queued messages were lost or wait was not measured",
        ));
    }
    Ok(sonic_rs::json!({
        "repetition": repetition,
        "limit": if byte_limited { "encoded-bytes" } else { "message-count" },
        "message_capacity": config.capacity,
        "encoded_byte_capacity": config.max_buffered_bytes,
        "encoded_message_bytes": encoded,
        "observed_queued_messages_high_water": high_water,
        "observed_queued_encoded_bytes_high_water": high_water * encoded,
        "additional_send_was_pending": true,
        "receiver_resume_unblocked_send": true,
        "queue_wait_us": waited,
        "messages_drained": high_water + 1,
        "wire_bytes": bytes,
        "status": "PASS",
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    futures::executor::block_on(async {
        let mut rows = Vec::new();
        for repetition in 0..3 {
            for byte_limited in [false, true] {
                rows.push(measure(repetition, byte_limited).await?);
            }
        }
        println!(
            "{}",
            sonic_rs::json!({
                "schemaVersion": 1, "status": "PASS", "cases": rows,
                "scope": "prepared identity channel only; queued messages/bytes; excludes pending future storage and H2 buffers; no socket/throughput claim",
            })
        );
        Ok(())
    })
}
