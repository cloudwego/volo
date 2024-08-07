use std::{fmt::Display, time::Duration};

use sysinfo::{ProcessRefreshKind, RefreshKind};
use tokio_util::sync::CancellationToken;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(3);
const DEFAULT_RSS_THRESHOLD: u64 = 1024 * 1024; // bytes

#[derive(Debug, Default)]
pub struct Usage {
    max_rss: u64,
    avg_rss: u64,
}

impl Usage {
    pub fn new(rss_list: &[u64]) -> Self {
        if rss_list.is_empty() {
            return Self::default();
        }
        let mut total_rss = 0;
        let mut max_rss = 0;
        for rss in rss_list {
            total_rss += *rss;
            if *rss > max_rss {
                max_rss = *rss;
            }
        }
        Self {
            max_rss,
            avg_rss: total_rss / rss_list.len() as u64,
        }
    }
}

impl Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "AVG: {} MB, MAX: {} MB",
            self.avg_rss / 1024 / 1024,
            self.max_rss / 1024 / 1024
        )
    }
}

pub async fn record_usage(mem_usage_list: &mut Vec<u64>, cancel: CancellationToken) {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let mut system = sysinfo::System::new_with_specifics(RefreshKind::everything().without_cpu());
    if system.process(pid).is_none() {
        panic!("process not found");
    }

    loop {
        tokio::select! {
            _ = tokio::time::sleep(DEFAULT_INTERVAL) => {
                system.refresh_process_specifics(pid, ProcessRefreshKind::new().with_memory());
                let mem_usage = system
                    .process(pid)
                    .unwrap()
                    .memory();
                if mem_usage > DEFAULT_RSS_THRESHOLD {
                    mem_usage_list.push(mem_usage as u64);
                }
            }
            _ = cancel.cancelled() => break,
        }
    }
}
