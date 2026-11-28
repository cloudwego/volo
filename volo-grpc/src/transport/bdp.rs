//! Shared threshold handshake for DATA producers and the H2 connection driver.

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
pub(crate) struct Sample {
    received: AtomicUsize,
    threshold: AtomicUsize,
}

impl Sample {
    pub(crate) fn new(window: u32) -> Self {
        Self {
            threshold: AtomicUsize::new((window as usize / 2).max(1)),
            ..Default::default()
        }
    }

    pub(crate) fn received(&self) -> usize {
        self.received.load(Ordering::SeqCst)
    }

    pub(crate) fn suspend(&self) {
        self.threshold.store(usize::MAX, Ordering::SeqCst);
    }

    /// The caller must check this returned count before sleeping. Either it sees
    /// DATA racing rearming, or record() sees the new threshold and wakes it.
    /// AcqRel on two independent atomics does not provide that guarantee.
    pub(crate) fn arm(&self, baseline: usize, window: u32) -> usize {
        self.threshold.store(
            baseline.saturating_add((window as usize / 2).max(1)),
            Ordering::SeqCst,
        );
        self.received()
    }

    /// True only for the DATA chunk crossing the current sampling threshold.
    pub(crate) fn record(&self, len: usize) -> bool {
        let previous = self.received.fetch_add(len, Ordering::SeqCst);
        let threshold = self.threshold.load(Ordering::SeqCst);
        previous < threshold && previous.saturating_add(len) >= threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_only_on_threshold_crossing_and_recheck_when_rearming() {
        let sample = Sample::new(1024);
        assert!(!sample.record(511));
        assert!(sample.record(1));
        assert!(!sample.record(100));
        sample.suspend();
        let before_pong = sample.received();
        assert!(!sample.record(4096));
        assert!(sample.arm(before_pong, 1024) - before_pong >= 512);
        let baseline = sample.received();
        assert_eq!(sample.arm(baseline, 1024), baseline);
        assert!(!sample.record(511));
        assert!(sample.record(1));
    }
}
