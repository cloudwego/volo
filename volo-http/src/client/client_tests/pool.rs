//! Test cases for the connection pool of the client

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::{body::BodyConversion, client::Client};

/// A response head and a body that is sent a bit later, so that the connection is still busy when
/// the response head arrives at the client.
const DELAYED_RESP: &[&[u8]] = &[b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n", b"hello"];
/// A response that announces more data than it ever sends, so the body never completes.
const PENDING_RESP: &[&[u8]] = &[b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\nhello"];

/// Spawn a minimal HTTP/1.1 server that counts how many connections were accepted.
///
/// For each request, the chunks of `resp` are written with a small delay between them.
async fn counting_server(resp: &'static [&'static [u8]]) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conns = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let conns = Arc::clone(&conns);
        async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                conns.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    // Requests without body only, so a read that contains the end of the header is
                    // enough for replying.
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        for chunk in resp {
                            if stream.write_all(chunk).await.is_err() {
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                });
            }
        }
    });

    (format!("http://{addr}/"), conns)
}

/// The connection is released to the pool only after the response body is finished, so it must be
/// reused by the following requests instead of being discarded as a closed one.
#[tokio::test]
async fn http1_conn_is_reused_after_body_is_consumed() {
    let (url, conns) = counting_server(DELAYED_RESP).await;
    let client = Client::builder().build().unwrap();

    for _ in 0..3 {
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.into_string().await.unwrap(), "hello");
        // Give the deferred release a chance to insert the connection into the pool.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(conns.load(Ordering::SeqCst), 1);
}

/// A connection whose response body is dropped before it is complete cannot be reused. Waiting for
/// it to become idle must not put it into the pool: the connection is closed and discarded, and
/// the next request opens a new one.
#[tokio::test]
async fn http1_conn_is_dropped_if_body_is_incomplete() {
    let (url, conns) = counting_server(PENDING_RESP).await;
    let client = Client::builder().build().unwrap();

    for _ in 0..2 {
        let resp = client.get(&url).send().await.unwrap();
        drop(resp);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

/// Spawn a minimal HTTP/1.1 server that replies with a body of `body_size` bytes and counts how
/// many connections were accepted.
///
/// The server keeps connections alive and replies once for every request it receives.
async fn bench_server(body_size: usize) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let conns = Arc::new(AtomicUsize::new(0));

    let head =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {body_size}\r\n\r\n").into_bytes();
    let body = vec![b'x'; body_size];
    let resp = Arc::new([head, body].concat());

    tokio::spawn({
        let conns = Arc::clone(&conns);

        async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                stream.set_nodelay(true).unwrap();

                conns.fetch_add(1, Ordering::SeqCst);

                let resp = Arc::clone(&resp);

                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];

                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(_) => {
                                if stream.write_all(&resp).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
        }
    });

    (format!("http://{addr}/"), conns)
}


/// A single benchmark configuration.
#[derive(Clone, Copy)]
struct BenchCase {
    body_size: usize,
    concurrency: usize,
    requests: usize,
    iterations: usize,
}


/// Results from one benchmark iteration.
struct BenchResult {
    connections: usize,
    requests: usize,
    elapsed: Duration,
    latencies: Vec<Duration>,
}


impl BenchResult {
    fn requests_per_connection(&self) -> f64 {
        self.requests as f64 / self.connections.max(1) as f64
    }

    fn requests_per_second(&self) -> f64 {
        self.requests as f64 / self.elapsed.as_secs_f64()
    }

    fn mib_per_second(&self, body_size: usize) -> f64 {
        (self.requests as f64 * body_size as f64)
            / self.elapsed.as_secs_f64()
            / (1024.0 * 1024.0)
    }

    fn percentile_ms(&self, percentile: usize) -> f64 {
        percentile_duration_ms(&self.latencies, percentile)
    }
}


/// Measure connection-pool reuse across different response body sizes and concurrency levels.
///
/// This benchmark intentionally runs a full body-size × concurrency matrix.
///
/// Body sizes:
///
///     64 KiB
///     128 KiB
///     256 KiB
///     512 KiB
///     1 MiB
///     2 MiB
///     4 MiB
///
/// Concurrency:
///
///     1
///     2
///     4
///     8
///     16
///     32
///     64
///     128
///
/// Each configuration is run multiple times with a warm-up request before measurement.
///
/// The most important metric for this benchmark is `connections`. A healthy connection pool
/// should create approximately one connection per simultaneously active request/worker, rather
/// than creating a new connection for every request.
///
/// Run with:
///
/// ```bash
/// cargo test -p volo-http --release --lib client_tests::pool::bench_conn_reuse -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark to be ran manually"]
async fn bench_conn_reuse() {
    const REQUESTS: usize = 1_000;
    const ITERATIONS: usize = 3;

    const BODY_SIZES: &[usize] = &[
        64 * 1024,
        128 * 1024,
        256 * 1024,
        512 * 1024,
        1024 * 1024,
        2 * 1024 * 1024,
        4 * 1024 * 1024,
    ];

    const CONCURRENCIES: &[usize] = &[1, 2, 4, 8, 16, 32, 64, 128];

    println!();
    println!("==============================================================");
    println!("             HTTP/1.1 CONNECTION POOL BENCHMARK");
    println!("==============================================================");
    println!();
    println!("Requests per iteration : {REQUESTS}");
    println!("Iterations             : {ITERATIONS}");
    println!("Body sizes              : 64 KiB -> 4 MiB");
    println!("Concurrency             : 1 -> 128");
    println!();

    for &body_size in BODY_SIZES {
        println!();
        println!(
            "--------------------------------------------------------------------------"
        );
        println!(
            " BODY SIZE: {}",
            format_bytes(body_size)
        );
        println!(
            "--------------------------------------------------------------------------"
        );

        println!(
            "{:>8} {:>10} {:>12} {:>12} {:>12} {:>10} {:>10} {:>10} {:>10}",
            "conc.",
            "requests",
            "connections",
            "req/conn",
            "req/s",
            "MiB/s",
            "p50 ms",
            "p99 ms",
            "max ms",
        );

        println!(
            "{:-<8} {:-<10} {:-<12} {:-<12} {:-<12} {:-<10} {:-<10} {:-<10} {:-<10}",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
        );

        for &concurrency in CONCURRENCIES {
            let case = BenchCase {
                body_size,
                concurrency,
                requests: REQUESTS,
                iterations: ITERATIONS,
            };

            let result = run_benchmark_case(case).await;

            println!(
                "{:>8} {:>10} {:>12} {:>12.1} {:>12.0} {:>10.1} {:>10.3} {:>10.3} {:>10.3}",
                concurrency,
                result.requests,
                result.connections,
                result.requests_per_connection(),
                result.requests_per_second(),
                result.mib_per_second(body_size),
                result.percentile_ms(50),
                result.percentile_ms(99),
                result
                    .latencies
                    .last()
                    .map(|latency| latency.as_secs_f64() * 1000.0)
                    .unwrap_or(0.0),
            );
        }
    }

    println!();
    println!("==============================================================");
    println!(" Benchmark complete");
    println!("==============================================================");
    println!();
}


/// Run one body-size/concurrency configuration for multiple iterations and return the median
/// iteration.
///
/// The median is selected by total elapsed time. Latency samples are taken from the median
/// iteration as well, keeping each row internally consistent.
async fn run_benchmark_case(case: BenchCase) -> BenchResult {
    let mut results = Vec::with_capacity(case.iterations);

    for iteration in 0..case.iterations {
        let result = run_benchmark_iteration(case).await;
        results.push(result);

        // Small separator between repeated iterations. This is intentionally not included in
        // the measurement.
        if iteration + 1 < case.iterations {
            tokio::task::yield_now().await;
        }
    }

    results.sort_unstable_by_key(|result| result.elapsed);

    results.remove(results.len() / 2)
}


/// Run one measured iteration of a benchmark case.
async fn run_benchmark_iteration(case: BenchCase) -> BenchResult {
    let (url, conns) = bench_server(case.body_size).await;

    // Warm up the server with a separate client. We deliberately do not use the benchmark
    // client's connection for measurement, otherwise the warm-up connection could make the
    // connection count appear smaller than the number of connections actually opened during
    // the measured workload.
    {
        let warmup_client = Client::builder().build().unwrap();

        let body = warmup_client
            .get(&url)
            .send()
            .await
            .unwrap()
            .into_bytes()
            .await
            .unwrap();

        assert_eq!(body.len(), case.body_size);

        drop(warmup_client);
    }

    // Give the warm-up connection a chance to close before measuring.
    tokio::task::yield_now().await;

    // Reset the connection counter after warm-up.
    conns.store(0, Ordering::SeqCst);

    let client = Client::builder().build().unwrap();

    let start = std::time::Instant::now();

    let mut workers = Vec::with_capacity(case.concurrency);

    for worker_id in 0..case.concurrency {
        let client = client.clone();
        let url = url.clone();

        let base = case.requests / case.concurrency;
        let extra = usize::from(worker_id < case.requests % case.concurrency);
        let worker_requests = base + extra;

        workers.push(tokio::spawn(async move {
            let mut latencies = Vec::with_capacity(worker_requests);

            for _ in 0..worker_requests {
                let request_start = std::time::Instant::now();

                let response = client.get(&url).send().await.unwrap();
                let body = response.into_bytes().await.unwrap();

                assert_eq!(body.len(), case.body_size);

                latencies.push(request_start.elapsed());
            }

            latencies
        }));
    }

    let mut latencies = Vec::with_capacity(case.requests);

    for worker in workers {
        latencies.extend(worker.await.unwrap());
    }

    let elapsed = start.elapsed();

    latencies.sort_unstable();

    let connections = conns.load(Ordering::SeqCst);

    assert_eq!(
        latencies.len(),
        case.requests,
        "benchmark completed the wrong number of requests"
    );

    BenchResult {
        connections,
        requests: latencies.len(),
        elapsed,
        latencies,
    }
}


fn percentile_duration_ms(latencies: &[Duration], percentile: usize) -> f64 {
    if latencies.is_empty() {
        return 0.0;
    }

    assert!(percentile <= 100);

    let index = ((latencies.len() - 1) * percentile) / 100;

    latencies[index].as_secs_f64() * 1000.0
}


fn format_bytes(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;

    if bytes >= MIB {
        format!("{:.0} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.0} KiB", bytes as f64 / KIB as f64)
    }
}