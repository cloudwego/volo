//! Process-wide requested allocation accounting for the isolated I10 profile.
//! This allocator is never installed in the throughput fixture's default build.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_json::{Value, json};

static CALLS: AtomicUsize = AtomicUsize::new(0);
static REQUESTED: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static SIZES: [AtomicUsize; usize::BITS as usize] =
    [const { AtomicUsize::new(0) }; usize::BITS as usize];

pub struct Allocator;

fn allocated(size: usize) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    REQUESTED.fetch_add(size, Ordering::Relaxed);
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
    let bucket = size.max(1).ilog2() as usize;
    SIZES[bucket].fetch_add(1, Ordering::Relaxed);
}

// SAFETY: Every pointer, layout and requested size is forwarded unchanged to
// System. A failed realloc leaves its old allocation and accounting intact.
unsafe impl GlobalAlloc for Allocator {
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
        FREES.fetch_add(1, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            FREES.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            allocated(size);
        }
        next
    }
}

pub fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}

pub fn snapshot() -> Value {
    // Read counters before allocating the JSON observation itself. The next
    // snapshot includes that small fixed observation cost, reported separately.
    let calls = CALLS.load(Ordering::Relaxed);
    let requested = REQUESTED.load(Ordering::Relaxed);
    let frees = FREES.load(Ordering::Relaxed);
    let live = LIVE.load(Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed);
    let sizes: [usize; usize::BITS as usize] =
        std::array::from_fn(|i| SIZES[i].load(Ordering::Relaxed));
    json!({"calls":calls,"requested_bytes":requested,"frees":frees,"live_requested_bytes":live,
        "peak_requested_bytes":peak,"size_buckets_log2":sizes.as_slice()})
}
