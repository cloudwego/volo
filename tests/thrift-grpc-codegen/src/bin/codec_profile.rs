//! Isolated, instrumented allocation/copy accounting; not a throughput baseline.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use futures::{StreamExt, stream};
use pilota::{Bytes, LinkedBytes, thrift::binary::TBinaryProtocol};
use thrift_grpc_codegen::volo_gen::{grpc_parent::*, grpc_thrift::*};
use volo_grpc::{
    Status,
    codec::{Encoder, compression::CompressionEncoding, thrift::ThriftEncoder},
};

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static REQUESTED: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;
fn allocated(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    REQUESTED.fetch_add(bytes, Ordering::Relaxed);
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
// Delegates every layout unchanged to System. Counters track requested bytes,
// not allocator metadata, fragmentation, RSS, or physical copies.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, new_size) };
        if !next.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            allocated(new_size);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct CountingEncoder<T> {
    inner: ThriftEncoder<T>,
    calls: Arc<AtomicUsize>,
}
impl<T: pilota::thrift::Message> Encoder for CountingEncoder<T> {
    type Item = T;
    type Error = Status;
    fn encode(&mut self, item: T, output: &mut LinkedBytes) -> Result<(), Status> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.inner.encode(item, output)
    }
}

async fn profile(
    payload_bytes: usize,
    count: usize,
    gzip: bool,
) -> Result<sonic_rs::Value, Status> {
    let payload = Bytes::from(vec![b'x'; payload_bytes]);
    let value = Message {
        payload: Some(payload.clone()),
        sequence: Some(1),
    };
    profile_value(value, &payload, count, gzip).await
}

async fn profile_value<T: pilota::thrift::Message + Clone + 'static>(
    value: T,
    payload: &[u8],
    count: usize,
    gzip: bool,
) -> Result<sonic_rs::Value, Status> {
    let pointer = payload.as_ptr() as usize;
    let payload_bytes = payload.len();
    let encoded_len = value.size(&mut TBinaryProtocol::new((), true));
    let calls = Arc::new(AtomicUsize::new(0));
    let initial_live = LIVE.load(Ordering::Relaxed);
    let initial_allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let initial_requested = REQUESTED.load(Ordering::Relaxed);
    PEAK.store(initial_live, Ordering::Relaxed);
    let begin = Instant::now();
    let compression = gzip.then_some(CompressionEncoding::Gzip(None));
    let encoder = CountingEncoder {
        inner: ThriftEncoder::default(),
        calls: calls.clone(),
    };
    let mut output = volo_grpc::codec::encode::encode_with_encoder(
        stream::iter(std::iter::repeat_n(Ok(value), count)),
        compression,
        encoder,
    );
    let (mut wire_bytes, mut shared_payload_bytes, mut data_frames) = (0usize, 0usize, 0usize);
    while let Some(frame) = output.next().await {
        if let Ok(data) = frame?.into_data() {
            wire_bytes += data.len();
            data_frames += 1;
            // Keep payload alive while comparing addresses, so allocator reuse
            // cannot masquerade as a shared node on the compressed path.
            if data.as_ptr() as usize == pointer && data.len() == payload.len() {
                shared_payload_bytes += data.len();
            }
        }
    }
    drop(output);
    let elapsed = begin.elapsed();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - initial_allocations;
    let requested = REQUESTED.load(Ordering::Relaxed) - initial_requested;
    let peak_extra = PEAK.load(Ordering::Relaxed).saturating_sub(initial_live);
    let retained_extra = LIVE.load(Ordering::Relaxed).saturating_sub(initial_live);
    let encoded_calls = calls.load(Ordering::Relaxed);
    if encoded_calls != count || (gzip && shared_payload_bytes != 0) {
        return Err(Status::internal("codec accounting invariant failed"));
    }
    Ok(
        sonic_rs::json!({"payload_bytes":payload_bytes,"messages":count,"compression":if gzip {"gzip"} else {"identity"},
        "encoder_constructions":1,"encoder_calls":encoded_calls,"allocations":allocations,"requested_bytes":requested,
        "peak_extra_requested_live_bytes":peak_extra,"retained_extra_requested_live_bytes":retained_extra,
        "encoded_message_bytes":encoded_len,"data_frames":data_frames,"wire_bytes":wire_bytes,"shared_payload_node_bytes":shared_payload_bytes,
        "known_concat_input_copy_bytes":0,"instrumented_elapsed_us":elapsed.as_micros() as u64}),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    futures::executor::block_on(async {
        // Warm each compression path before taking allocator differences.
        for gzip in [false, true] {
            profile(64, 1, gzip).await?;
        }
        let mut rows = Vec::new();
        let mut unary_rows = Vec::new();
        for repetition in 0..3 {
            for payload in [64, 1024, 65536, 1048576] {
                for gzip in [false, true] {
                    let mut result = profile(payload, 100, gzip).await?;
                    result["repetition"] = repetition.into();
                    rows.push(result);
                    let payload = Bytes::from(vec![b'x'; payload]);
                    let value = Message {
                        payload: Some(payload.clone()),
                        sequence: Some(1),
                    };
                    let text =
                        pilota::FastStr::from_string(String::from_utf8(payload.to_vec()).unwrap());
                    for (shape, mut result) in [
                        (
                            "args",
                            profile_value(
                                InteropUnaryArgsSend {
                                    request: value.clone(),
                                },
                                &payload,
                                100,
                                gzip,
                            )
                            .await?,
                        ),
                        (
                            "success-result",
                            profile_value(InteropUnaryResultSend::Ok(value), &payload, 100, gzip)
                                .await?,
                        ),
                        (
                            "declared-exception-result",
                            profile_value(
                                InteropThrowResultSend::Failure(Failure {
                                    message: Some(text.clone()),
                                }),
                                text.as_bytes(),
                                100,
                                gzip,
                            )
                            .await?,
                        ),
                    ] {
                        result["shape"] = shape.into();
                        result["input_payload_bytes"] = payload.len().into();
                        result["repetition"] = repetition.into();
                        unary_rows.push(result);
                    }
                }
            }
            for gzip in [false, true] {
                let mut result =
                    profile_value(InteropZeroResultSend::Ok(()), &[], 100, gzip).await?;
                result["shape"] = "void-result".into();
                result["input_payload_bytes"] = 0.into();
                result["repetition"] = repetition.into();
                unary_rows.push(result);
            }
        }
        println!(
            "{}",
            sonic_rs::json!({"schemaVersion":1,"status":"PASS","scope":"single-thread instrumented encoding allocation/copy accounting; input construction excluded; no socket/RSS/throughput claim","cases":rows,"unary_cases":unary_rows})
        );
        Ok(())
    })
}
