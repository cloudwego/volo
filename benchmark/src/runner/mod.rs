use std::{num::NonZeroU32, sync::Arc, time::Instant};

use chrono::Local;
use counter::Counter;
use governor::{DefaultDirectRateLimiter, Quota};
use tokio::task::JoinSet;

use crate::benchmark::echo::{EchoServerClient, Request};

pub mod counter;
pub mod processor;

#[derive(Clone)]
pub struct Runner {
    inner: Arc<Inner>,
}

pub struct Inner {
    counter: Counter,
    limiter: Option<DefaultDirectRateLimiter>,
}

impl Runner {
    pub fn new(qps: usize) -> Self {
        Runner {
            inner: Arc::new(Inner {
                counter: Counter::new(),
                limiter: if qps > 0 {
                    Some(DefaultDirectRateLimiter::direct(Quota::per_second(
                        NonZeroU32::new(qps as u32).unwrap(),
                    )))
                } else {
                    None
                },
            }),
        }
    }

    pub async fn benching(
        &self,
        client: EchoServerClient,
        req: Request,
        concurrent: usize,
        total: usize,
    ) {
        let mut set = JoinSet::new();
        self.inner.counter.reset(total);
        for _ in 0..concurrent {
            let client = client.clone();
            let req = req.clone();
            let runner = self.clone();
            set.spawn(async move {
                loop {
                    let idx = runner.inner.counter.idx();
                    if idx >= total {
                        return;
                    }
                    if let Some(limiter) = &runner.inner.limiter {
                        limiter.until_ready().await;
                    }
                    let now = std::time::Instant::now();
                    let resp = client.echo(req.clone()).await;
                    let cost = now.elapsed().as_nanos();
                    runner
                        .inner
                        .counter
                        .add_record(idx, resp.is_err(), cost as usize);
                }
            });
        }
        while (set.join_next().await).is_some() {}
    }

    pub async fn warmup(
        &self,
        client: EchoServerClient,
        req: Request,
        concurrent: usize,
        total: usize,
    ) {
        self.benching(client, req, concurrent, total).await;
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        client: EchoServerClient,
        req: Request,
        concurrent: usize,
        total: usize,
        qps: usize,
        sleep_time: usize,
        echo_size: usize,
        title: &str,
    ) {
        println!(
            "Info: {} start benching {}, concurrent: {}, qps: {}, total: {}, sleep: {}",
            title,
            Local::now(),
            concurrent,
            qps,
            total,
            sleep_time
        );
        let now = Instant::now();
        self.benching(client, req, concurrent, total).await;
        let elapsed = now.elapsed().as_nanos();
        self.inner
            .counter
            .report(title, elapsed as usize, concurrent, total, echo_size, qps)
    }
}
