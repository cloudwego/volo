use std::{fmt::Display, time::Duration};

use sysinfo::{ProcessRefreshKind, RefreshKind};
use tokio_util::sync::CancellationToken;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_USAGE_THRESHOLD: f32 = 10.0;

#[derive(Default)]
pub struct Usage {
    min: f32,
    max: f32,
    avg: f32,
    p50: f32,
    p90: f32,
    p99: f32,
}

impl Usage {
    pub fn new(stats: &mut [f32]) -> Self {
        if stats.is_empty() {
            return Self::default();
        }

        let mut stats = stats;
        stats.sort_by(|a, b| a.total_cmp(b));
        let mut length = stats.len();
        if length > 3 {
            stats = &mut stats[1..length - 1];
            length -= 2;
        }
        let f_len = stats.len() as f32;
        let tp50_index = (f_len * 0.5) as usize;
        let tp90_index = (f_len * 0.9) as usize;
        let tp99_index = (f_len * 0.99) as usize;

        let mut usage = Self::default();
        if tp50_index > 0 {
            usage.p50 = stats[tp50_index - 1];
        }
        if tp90_index > tp50_index {
            usage.p90 = stats[tp90_index - 1];
        } else {
            usage.p90 = usage.p50;
        }
        if tp99_index > tp90_index {
            usage.p99 = stats[tp99_index - 1];
        } else {
            usage.p99 = usage.p90;
        }

        let sum: f32 = stats.iter().sum();
        usage.avg = sum / f_len;

        usage.min = stats[0];
        usage.max = stats[length - 1];

        usage
    }
}

impl Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "MIN: {:.2}%, TP50: {:.2}%, TP90: {:.2}%, TP99: {:.2}%, MAX: {:.2}%, AVG:{:.2}%",
            self.min, self.p50, self.p90, self.p99, self.max, self.avg
        )
    }
}

pub async fn record_usage(cpu_usage_list: &mut Vec<f32>, cancel: CancellationToken) {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let mut system =
        sysinfo::System::new_with_specifics(RefreshKind::everything().without_memory());

    if system.process(pid).is_none() {
        panic!("process not found");
    }

    loop {
        tokio::select! {
            _ = tokio::time::sleep(DEFAULT_INTERVAL) => {
                system.refresh_process_specifics(pid, ProcessRefreshKind::new().with_cpu());
                let cpu_usage = system
                    .process(pid)
                    .unwrap()
                    .cpu_usage();
                if cpu_usage > DEFAULT_USAGE_THRESHOLD {
                    cpu_usage_list.push(cpu_usage);
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
}
