use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicUsize, Ordering},
};

use chrono::Local;

#[derive(Default)]
pub struct Counter {
    total: AtomicUsize,
    failed: AtomicUsize,
    costs: UnsafeCell<Vec<usize>>,
}

unsafe impl Sync for Counter {}

impl Counter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self, total: usize) {
        self.total.store(0, Ordering::Relaxed);
        self.failed.store(0, Ordering::Relaxed);
        unsafe {
            *self.costs.get() = vec![0; total];
        }
    }

    pub fn add_record(&self, idx: usize, err: bool, cost: usize) {
        unsafe {
            (*self.costs.get())[idx] = cost;
        }
        if err {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn idx(&self) -> usize {
        self.total.fetch_add(1, Ordering::Relaxed)
    }

    pub fn report(
        &self,
        title: &str,
        totalns: usize,
        concurrent: usize,
        total: usize,
        echo_size: usize,
        qps: usize,
    ) {
        let ms = 1_000_000;
        let sec = 1_000_000_000;
        println!(
            "Info: [{}]: finish benching [{}], took {} ms for {} requests",
            title,
            Local::now(),
            totalns / ms,
            self.total.load(Ordering::Relaxed),
        );
        println!(
            "Info: [{}]: requests total: {}, failed: {}",
            title,
            self.total.load(Ordering::Relaxed),
            self.failed.load(Ordering::Relaxed)
        );

        let tps = if totalns < sec {
            (self.total.load(Ordering::Relaxed) * sec) as f64 / totalns as f64
        } else {
            self.total.load(Ordering::Relaxed) as f64 / (totalns as f64 / sec as f64)
        };

        let costs = unsafe { &mut *self.costs.get() };
        costs.sort_unstable();
        let tp99 = costs[(costs.len() as f64 * 0.99) as usize];
        let tp999 = costs[(costs.len() as f64 * 0.999) as usize];

        let result = if tp999 / 1_000 < 1 {
            format!(
                "Info: [{}]: TPS: {:.2}, TP99: {:.2}us, TP999: {:.2}us (b={} Byte, c={}, qps={}, \
                 n={})",
                title,
                tps,
                tp99 as f64 / 1_000.0,
                tp999 as f64 / 1_000.0,
                echo_size,
                concurrent,
                qps,
                total
            )
        } else {
            format!(
                "Info: [{}]: TPS: {:.2}, TP99: {:.2}ms, TP999: {:.2}ms (b={} Byte, c={}, qps={}, \
                 n={})",
                title,
                tps,
                tp99 as f64 / 1_000_000.0,
                tp999 as f64 / 1_000_000.0,
                echo_size,
                concurrent,
                qps,
                total
            )
        };
        println!("{}", result);
    }
}
