//! Optional client timing for benchmark fixtures, sampled through the public hooks.
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use volo_grpc::{
    Code,
    metadata::MetadataMap,
    streaming::{StreamContext, StreamHook, StreamHookFactory, StreamStatus},
};

const MAX_SAMPLES: usize = 8192;

#[derive(Clone)]
pub struct PhaseRecorder(Arc<Mutex<Samples>>);

struct Samples {
    warmup: usize,
    finished: usize,
    errors: usize,
    invalid_order: usize,
    values: Vec<Timing>,
}

#[derive(Clone, Copy, Default)]
struct Timing {
    header_us: Option<u64>,
    first_message_us: Option<u64>,
    last_message_us: Option<u64>,
    finished_us: u64,
}

struct CallTiming {
    started: Instant,
    timing: Mutex<Timing>,
    recorder: PhaseRecorder,
}

impl PhaseRecorder {
    pub fn new(warmup: usize) -> Self {
        Self(Arc::new(Mutex::new(Samples {
            warmup,
            finished: 0,
            errors: 0,
            invalid_order: 0,
            values: Vec::with_capacity(MAX_SAMPLES),
        })))
    }

    pub fn summary(&self) -> sonic_rs::Value {
        let samples = self.0.lock().unwrap();
        sonic_rs::json!({
            "scope": "client hooks, measured after warmup, clock and locking overhead included",
            "origin": "per-call hook construction",
            "finished": samples.finished,
            "errors": samples.errors,
            "invalid_order": samples.invalid_order,
            "sample_limit": MAX_SAMPLES,
            "sample_count": samples.values.len(),
            "header_us": histogram(samples.values.iter().filter_map(|value| value.header_us)),
            "first_message_us": histogram(samples.values.iter().filter_map(|value| value.first_message_us)),
            "last_message_us": histogram(samples.values.iter().filter_map(|value| value.last_message_us)),
            "finished_us": histogram(samples.values.iter().map(|value| value.finished_us)),
        })
    }
}

impl StreamHookFactory for PhaseRecorder {
    fn create(&self, _: &StreamContext) -> Arc<dyn StreamHook> {
        Arc::new(CallTiming {
            started: Instant::now(),
            timing: Mutex::default(),
            recorder: self.clone(),
        })
    }
}

impl CallTiming {
    fn elapsed_us(&self) -> u64 {
        self.started
            .elapsed()
            .as_micros()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

impl StreamHook for CallTiming {
    fn on_header(&self, _: &StreamContext, _: &MetadataMap) {
        self.timing.lock().unwrap().header_us = Some(self.elapsed_us());
    }

    fn on_recv(&self, _: &StreamContext, _: usize) {
        let elapsed = self.elapsed_us();
        let mut timing = self.timing.lock().unwrap();
        timing.first_message_us.get_or_insert(elapsed);
        timing.last_message_us = Some(elapsed);
    }

    fn on_finish(&self, _: &StreamContext, status: &StreamStatus) {
        let mut value = *self.timing.lock().unwrap();
        value.finished_us = self.elapsed_us();
        let mut samples = self.recorder.0.lock().unwrap();
        if samples.warmup > 0 {
            samples.warmup -= 1;
            return;
        }
        if let (Some(header), Some(first), Some(last)) = (
            value.header_us,
            value.first_message_us,
            value.last_message_us,
        ) {
            samples.invalid_order +=
                usize::from(header > first || first > last || last > value.finished_us);
        }
        samples.errors += usize::from(status.status.code() != Code::Ok);
        if samples.values.len() < MAX_SAMPLES {
            samples.values.push(value);
        } else {
            let slot = samples.finished % MAX_SAMPLES;
            samples.values[slot] = value;
        }
        samples.finished += 1;
    }
}

fn histogram(values: impl Iterator<Item = u64>) -> sonic_rs::Value {
    let mut values: Vec<_> = values.collect();
    values.sort_unstable();
    let percentile = |percent| {
        values
            .get(values.len().saturating_sub(1) * percent / 100)
            .copied()
    };
    sonic_rs::json!({"count": values.len(), "p50": percentile(50), "p99": percentile(99)})
}
