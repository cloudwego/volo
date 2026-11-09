//! Common behavior used by the local loopback and Go interoperability drivers.

use futures::{StreamExt, stream};
use volo_grpc::{Request, Response, Status, codec::thrift::ThriftRecvStream};

use crate::volo_gen::{grpc_parent::*, grpc_thrift::*};

pub struct Handler;

static EXPECT_COMPRESSION: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
pub fn expect_compression(gzip: bool) {
    let _ = EXPECT_COMPRESSION.set(gzip);
}
fn check_compression(metadata: &volo_grpc::metadata::MetadataMap) -> Result<(), Status> {
    let Some(gzip) = EXPECT_COMPRESSION.get() else {
        return Ok(());
    };
    let actual = metadata
        .get("grpc-encoding")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if actual != if *gzip { "gzip" } else { "" } {
        return Err(Status::internal(
            "wire compression differs from fixture configuration",
        ));
    }
    Ok(())
}

static FORWARD: std::sync::OnceLock<InteropClient> = std::sync::OnceLock::new();
pub fn forward_to(client: InteropClient) {
    assert!(FORWARD.set(client).is_ok(), "forwarding configured once");
}

type ReplyStream = volo_grpc::thrift::SendStream<Message>;

fn server_replies(request: Message) -> ReplyStream {
    ReplyStream::from_stream(stream::iter((0..3).map(move |sequence| {
        Ok(Message {
            payload: request.payload.clone(),
            sequence: Some(sequence),
        })
    })))
}

impl Interop for Handler {
    async fn inherited(&self, request: Request<Message>) -> Result<Response<ReplyStream>, Status> {
        Ok(Response::new(server_replies(request.into_inner())))
    }

    async fn unary(
        &self,
        request: Request<InteropUnaryRequest>,
    ) -> Result<Response<Message>, Status> {
        check_compression(request.metadata())?;
        Ok(Response::new(request.into_inner().request))
    }

    async fn explicit_unary(
        &self,
        request: Request<InteropExplicitUnaryRequest>,
    ) -> Result<Response<Message>, Status> {
        check_compression(request.metadata())?;
        Ok(Response::new(request.into_inner().request))
    }

    async fn zero(&self, _: Request<InteropZeroRequest>) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn multi(&self, request: Request<InteropMultiRequest>) -> Result<Response<i64>, Status> {
        let request = request.into_inner();
        Ok(Response::new(i64::from(request.left) + request.right))
    }

    async fn container(
        &self,
        request: Request<InteropContainerRequest>,
    ) -> Result<Response<Vec<i64>>, Status> {
        Ok(Response::new(request.into_inner().values))
    }

    async fn throw(
        &self,
        request: Request<InteropThrowRequest>,
    ) -> Result<Response<Result<Message, InteropThrowException>>, Status> {
        let request = request.into_inner().request;
        if let Some(client) = FORWARD.get() {
            if request.payload.as_deref() == Some(b"biz") {
                println!("{}", sonic_rs::json!({"event":"biz-forwarded"}));
            }
            return client.throw(InteropThrowRequest { request }).await;
        }
        if request.payload.as_deref() == Some(b"biz") {
            println!("{}", sonic_rs::json!({"event":"biz-emitted"}));
            return Err(volo_grpc::thrift::BizStatus {
                code: 409,
                message: "失败：\"\\\n".into(),
                extra: [("中文\"\\".into(), "值\n\t\0".into())].into(),
            }
            .into_status());
        }
        Ok(Response::new(
            match request.payload.as_deref().unwrap_or_default() {
                b"failure" => Err(InteropThrowException::Failure(Failure {
                    message: Some("declared failure".into()),
                })),
                b"other" => Err(InteropThrowException::Other(Other { code: Some(7) })),
                _ => Ok(request),
            },
        ))
    }

    async fn void_throw(
        &self,
        _: Request<InteropVoidThrowRequest>,
    ) -> Result<Response<Result<(), InteropVoidThrowException>>, Status> {
        Ok(Response::new(Err(InteropVoidThrowException::Failure(
            Failure {
                message: Some("void failure".into()),
            },
        ))))
    }

    async fn client(
        &self,
        request: Request<ThriftRecvStream<Message>>,
    ) -> Result<Response<Message>, Status> {
        check_compression(request.metadata())?;
        let mut messages = request.into_inner();
        let mut payload = Vec::new();
        let mut count = 0;
        while let Some(message) = messages.message().await? {
            payload.extend_from_slice(message.payload.as_deref().unwrap_or_default());
            count += 1;
        }
        Ok(Response::new(Message {
            payload: Some(payload.into()),
            sequence: Some(count),
        }))
    }

    async fn server(&self, request: Request<Message>) -> Result<Response<ReplyStream>, Status> {
        check_compression(request.metadata())?;
        Ok(Response::new(server_replies(request.into_inner())))
    }

    async fn bidi(
        &self,
        request: Request<ThriftRecvStream<Message>>,
    ) -> Result<Response<ReplyStream>, Status> {
        check_compression(request.metadata())?;
        let cx = request
            .extensions()
            .get::<volo_grpc::streaming::StreamContext>()
            .unwrap()
            .clone();
        let mut messages = request.into_inner();
        let first = messages.message().await?;
        if let Some(first) = first {
            if let Some(command) = first
                .payload
                .as_deref()
                .and_then(|value| std::str::from_utf8(value).ok())
                .filter(|value| value.starts_with("lifecycle|"))
            {
                let parts = command.split('|').collect::<Vec<_>>();
                if parts.len() != 3 {
                    return Err(Status::invalid_argument("bad lifecycle command"));
                }
                let id = parts[1].to_owned();
                let phase = parts[2].to_owned();
                let guard = LifecycleDrop {
                    id: id.clone(),
                    kind: "handler-producer",
                };
                let (started, started_rx) = tokio::sync::oneshot::channel();
                let child_id = id.clone();
                cx.spawn_scoped(async move {
                    let _guard = LifecycleDrop {
                        id: child_id,
                        kind: "managed-child",
                    };
                    let _ = started.send(());
                    futures::future::pending::<()>().await;
                });
                started_rx
                    .await
                    .map_err(|_| Status::internal("managed child did not start"))?;
                use metainfo::Forward;
                let (transient, persistent) = cx.with_metainfo(|info| {
                    (
                        info.get_upstream("INTEROP_ONCE"),
                        info.get_persistent("INTEROP_PERSIST"),
                    )
                });
                println!(
                    "{}",
                    sonic_rs::json!({"event":"started", "id":id, "phase":phase, "transient":transient.as_deref().unwrap_or_default(), "persistent":persistent.as_deref().unwrap_or_default(), "remaining_ms":cx.deadline().map(|deadline| deadline.remaining().as_millis() as i64).unwrap_or(-1)})
                );
                let downstream = if let Some(client) = FORWARD.get() {
                    // Deliberately consume parent budget before the next hop.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let (mut tx, input) = volo_grpc::thrift::channel(32);
                    tx.send(&first).await?;
                    let response = client.bidi(input).await?;
                    let mut response = response.into_inner();
                    if phase != "before" && response.message().await?.as_ref() != Some(&first) {
                        return Err(Status::internal(
                            "downstream lifecycle acknowledgement differs",
                        ));
                    }
                    Some((tx, response))
                } else {
                    None
                };
                if phase == "before" {
                    let _guard = guard;
                    let _messages = messages;
                    let _downstream = downstream;
                    futures::future::pending::<()>().await;
                    unreachable!();
                }
                let pending = stream::once(async move {
                    let _guard = guard;
                    let _messages = messages;
                    let _downstream = downstream;
                    futures::future::pending::<Result<Message, Status>>().await
                });
                return Ok(Response::new(ReplyStream::from_stream(
                    stream::iter([Ok(first)]).chain(pending),
                )));
            }
            Ok(Response::new(ReplyStream::from_stream(
                stream::iter([Ok(first)]).chain(messages),
            )))
        } else {
            Ok(Response::new(ReplyStream::from_stream(stream::empty())))
        }
    }
}

struct LifecycleDrop {
    id: String,
    kind: &'static str,
}
impl Drop for LifecycleDrop {
    fn drop(&mut self) {
        println!(
            "{}",
            sonic_rs::json!({"event":"exited", "id":self.id, "kind":self.kind})
        );
    }
}

/// Control-driven cancellation verifies actual peer Drop events in the shared runner.
pub async fn lifecycle_client(
    client: &InteropClient,
    scenario: &str,
    id: &str,
) -> Result<volo_grpc::Code, Status> {
    use tokio::io::AsyncBufReadExt;
    let (mut tx, input) = volo_grpc::thrift::channel::<Message>(32);
    let before = scenario.ends_with("before");
    let timed = scenario.starts_with("deadline-");
    let message = Message {
        payload: Some(
            format!("lifecycle|{id}|{}", if before { "before" } else { "after" })
                .into_bytes()
                .into(),
        ),
        sequence: None,
    };
    tx.send(&message).await?;
    let control = async {
        let mut line = String::new();
        tokio::io::BufReader::new(tokio::io::stdin())
            .read_line(&mut line)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        if line.trim() != "cancel" {
            return Err(Status::invalid_argument("missing cancellation control"));
        }
        Ok::<(), Status>(())
    };
    let code = if before {
        println!("{}", sonic_rs::json!({"event":"armed", "id":id}));
        let mut call = Box::pin(client.bidi(input));
        tokio::select! {
            result = &mut call => {
                let status = result.err().ok_or_else(|| Status::internal("pending handler returned success"))?;
                println!("{}", sonic_rs::json!({"event":"observed-error","id":id,"code":status.code() as i32,"raw_code":status.raw_grpc_code(),"message":status.message()}));
                status.code()
            },
            controlled = control, if !timed => { controlled?; volo_grpc::Code::Cancelled },
        }
    } else {
        let response = client.bidi(input).await?;
        let cx = response
            .extensions()
            .get::<volo_grpc::streaming::StreamContext>()
            .unwrap()
            .clone();
        let mut responses = response.into_inner();
        if responses.message().await?.as_ref() != Some(&message) {
            return Err(Status::internal("lifecycle acknowledgement differs"));
        }
        if scenario == "half-close-cancel" {
            drop(tx);
        }
        println!("{}", sonic_rs::json!({"event":"armed", "id":id}));
        if !timed {
            control.await?;
        }
        let code = if scenario == "drop-response" {
            drop(responses);
            cx.status()
                .ok_or_else(|| Status::internal("response Drop did not finish the call"))?
                .status
                .code()
        } else {
            if !timed {
                cx.cancel();
            }
            let status = responses
                .message()
                .await
                .err()
                .ok_or_else(|| Status::internal("expected cancellation failure"))?;
            println!(
                "{}",
                sonic_rs::json!({"event":"observed-error","id":id,"code":status.code() as i32,"raw_code":status.raw_grpc_code(),"message":status.message()})
            );
            status.code()
        };
        cx.children_reclaimed().await;
        code
    };
    let expected = if timed {
        volo_grpc::Code::DeadlineExceeded
    } else {
        volo_grpc::Code::Cancelled
    };
    if code != expected {
        return Err(Status::internal(format!(
            "expected {expected:?}, got {code:?}"
        )));
    }
    Ok(code)
}

/// Observe a deliberately constructed raw H2 response. The runner, rather
/// than this peer, decides whether the observed result matches the case.
pub async fn response_probe(client: &InteropClient, mode: &str) -> sonic_rs::Value {
    let message = Message {
        payload: None,
        sequence: None,
    };
    let mut received = 0;
    let result: Result<(), Status> = async {
        match mode {
            "unary" => {
                client
                    .unary(InteropUnaryRequest { request: message })
                    .await?;
                received = 1;
            }
            "void" => {
                client.zero(InteropZeroRequest {}).await?;
                received = 1;
            }
            "client" => {
                client.client(stream::iter([message])).await?;
                received = 1;
            }
            "server" | "bidi" => {
                let response = if mode == "server" {
                    client.server(message).await?
                } else {
                    client.bidi(stream::iter([message])).await?
                };
                let mut messages = response.into_inner();
                while messages.message().await?.is_some() {
                    received += 1;
                }
            }
            _ => return Err(Status::invalid_argument("unknown response probe mode")),
        }
        Ok(())
    }
    .await;
    let status = result.err().unwrap_or_else(|| Status::ok(""));
    let (biz, biz_error) = match volo_grpc::thrift::BizStatus::from_status(&status) {
        Ok(Some(biz)) => (
            Some(sonic_rs::json!({"code":biz.code, "message":biz.message, "extra":biz.extra})),
            None,
        ),
        Ok(None) => (None, None),
        Err(error) => (None, Some(error.to_string())),
    };
    sonic_rs::json!({"schemaVersion":1, "event":"result", "status":"OBSERVED", "code":status.code() as i32, "messages":received, "message":status.message(), "raw_code":status.raw_grpc_code().map(|value| String::from_utf8_lossy(value).into_owned()), "details_hex":status.details().iter().map(|byte| format!("{byte:02x}")).collect::<String>(), "biz":biz, "biz_error":biz_error})
}

async fn load_exchange(
    client: &InteropClient,
    payload: pilota::Bytes,
    cancel: bool,
) -> Result<(), Status> {
    let (mut sender, input) = volo_grpc::thrift::channel(1);
    let message = Message {
        payload: Some(payload),
        sequence: Some(1),
    };
    sender.send(&message).await?;
    let mut response = client.bidi(input).await?.into_inner();
    if response.message().await?.as_ref() != Some(&message) {
        return Err(Status::internal("load echo differs"));
    }
    if cancel {
        drop((response, sender));
        return Ok(());
    }
    drop(sender);
    if response.message().await?.is_some() {
        return Err(Status::internal("extra load response"));
    }
    Ok(())
}

/// Cross the configured ping period without application DATA/receives, then
/// resume the same bidi or observe Kitex's excessive-ping connection failure.
pub async fn verify_keepalive(
    client: &InteropClient,
    excessive: bool,
) -> Result<sonic_rs::Value, Status> {
    let (mut sender, input) = volo_grpc::thrift::channel(2);
    let message = Message {
        payload: Some("keepalive".into()),
        sequence: Some(1),
    };
    sender.send(&message).await?;
    let response = client.bidi(input).await?;
    let context = response
        .extensions()
        .get::<volo_grpc::streaming::StreamContext>()
        .unwrap()
        .clone();
    let mut response = response.into_inner();
    if response.message().await?.as_ref() != Some(&message) {
        return Err(Status::internal("initial keepalive echo differs"));
    }
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    let mut failure = None;
    if excessive {
        let status = response
            .message()
            .await
            .expect_err("excessive pings must close Kitex connection");
        if status.raw_grpc_code().is_some()
            || matches!(
                status.code(),
                volo_grpc::Code::Ok
                    | volo_grpc::Code::Cancelled
                    | volo_grpc::Code::DeadlineExceeded
            )
        {
            return Err(Status::internal(
                "excessive pings did not produce a transport failure",
            ));
        }
        failure = Some(status.message().to_owned());
    } else {
        if context.status().is_some() {
            return Err(Status::internal("valid idle bidi terminated"));
        }
        sender.send(&message).await?;
        if response.message().await?.as_ref() != Some(&message) {
            return Err(Status::internal("resumed keepalive echo differs"));
        }
        drop(sender);
        if response.message().await?.is_some() {
            return Err(Status::internal("extra keepalive echo"));
        }
    }
    Ok(
        sonic_rs::json!({"schemaVersion":1,"event":"result","status":"PASS","scenario":if excessive {"excessive"} else {"valid"},"idle_ms":600,"transport_failure":failure}),
    )
}

/// Bounded local resource/load fixture. Steady-state latency and resource
/// recovery are separate outputs; allocator caching is not called a leak.
pub async fn run_load(
    client: InteropClient,
    duration: std::time::Duration,
    concurrency: usize,
    payload_bytes: usize,
    cooldown: u64,
) -> Result<sonic_rs::Value, Status> {
    if concurrency > 128 {
        return Err(Status::invalid_argument("resource load concurrency limit"));
    }
    run_load_kind(
        client,
        duration,
        concurrency,
        payload_bytes,
        cooldown,
        "mixed-drop",
    )
    .await
}

/// A new-capability measurement, not a cross-protocol throughput comparison.
pub async fn run_benchmark(
    client: InteropClient,
    duration: std::time::Duration,
    concurrency: usize,
    payload_bytes: usize,
    mode: &'static str,
) -> Result<sonic_rs::Value, Status> {
    if !matches!(mode, "unary" | "client" | "server" | "bidi") {
        return Err(Status::invalid_argument("benchmark mode"));
    }
    run_load_kind(client, duration, concurrency, payload_bytes, 0, mode).await
}

async fn benchmark_exchange(
    client: &InteropClient,
    payload: pilota::Bytes,
    cancel: bool,
    mode: &str,
) -> Result<(), Status> {
    let message = Message {
        payload: Some(payload),
        sequence: Some(1),
    };
    match mode {
        "unary" => {
            let response = client
                .unary(InteropUnaryRequest {
                    request: message.clone(),
                })
                .await?
                .into_inner();
            if response != message {
                return Err(Status::internal("benchmark unary differs"));
            }
        }
        "client" => {
            let response = client
                .client(stream::iter([
                    message.clone(),
                    message.clone(),
                    message.clone(),
                ]))
                .await?
                .into_inner();
            let expected = message.payload.as_deref().unwrap_or_default();
            let actual = response.payload.as_deref().unwrap_or_default();
            if response.sequence != Some(3)
                || actual.len() != expected.len() * 3
                || (!expected.is_empty()
                    && !actual.chunks(expected.len()).all(|value| value == expected))
            {
                return Err(Status::internal("benchmark client aggregation differs"));
            }
        }
        "server" => {
            let mut response = client.server(message.clone()).await?.into_inner();
            for index in 0..3 {
                let mut expected = message.clone();
                expected.sequence = Some(index);
                if response.message().await?.as_ref() != Some(&expected) {
                    return Err(Status::internal("benchmark server response differs"));
                }
            }
            if response.message().await?.is_some() {
                return Err(Status::internal("extra benchmark response"));
            }
        }
        _ => return load_exchange(client, message.payload.unwrap(), cancel).await,
    }
    Ok(())
}

async fn run_load_kind(
    client: InteropClient,
    duration: std::time::Duration,
    concurrency: usize,
    payload_bytes: usize,
    cooldown: u64,
    mode: &'static str,
) -> Result<sonic_rs::Value, Status> {
    use std::time::Duration;
    if concurrency == 0
        || concurrency > 256
        || payload_bytes > 1024 * 1024
        || duration.is_zero()
        || duration > Duration::from_secs(3600)
        || cooldown > 120
    {
        return Err(Status::invalid_argument("load fixture limits"));
    }
    let payload = pilota::Bytes::from(vec![b'x'; payload_bytes]);
    for _ in 0..32 {
        benchmark_exchange(&client, payload.clone(), false, mode).await?;
    }
    let baseline = resource_snapshot();
    let started = tokio::time::Instant::now();
    let until = started + duration;
    let mut workers = tokio::task::JoinSet::new();
    for worker in 0..concurrency {
        let client = client.clone();
        let payload = payload.clone();
        workers.spawn(async move {
            let (mut calls, mut cancelled, mut errors) = (0u64, 0u64, 0u64);
            let mut samples = Vec::with_capacity(8192);
            let mut error_samples = Vec::new();
            let cancel_phase = (10 - worker as u64 % 10) % 10;
            while tokio::time::Instant::now() < until {
                let tick = tokio::time::Instant::now();
                let cancel = mode == "mixed-drop" && calls % 10 == cancel_phase;
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    benchmark_exchange(&client, payload.clone(), cancel, mode),
                )
                .await
                {
                    Ok(Ok(())) => {
                        if cancel {
                            cancelled += 1;
                        }
                    }
                    failure => {
                        errors += 1;
                        if error_samples.len() < 4 {
                            let status = match failure {
                                Ok(Err(status)) => status,
                                Err(_) => Status::deadline_exceeded("benchmark 3s RPC limit"),
                                Ok(Ok(())) => unreachable!(),
                            };
                            error_samples.push(sonic_rs::json!({
                                "code": status.code() as i32,
                                "raw_code": status.raw_grpc_code(),
                                "message": status.message().chars().take(256).collect::<String>(),
                            }));
                        }
                    }
                }
                let elapsed = tick.elapsed().as_micros() as u64;
                if samples.len() < 8192 {
                    samples.push(elapsed);
                } else {
                    samples[(calls as usize) % 8192] = elapsed;
                }
                calls += 1;
            }
            (calls, cancelled, errors, samples, error_samples)
        });
    }
    drop(client);
    let mut snapshots = vec![baseline.clone()];
    let (mut calls, mut cancelled, mut errors) = (0u64, 0u64, 0u64);
    let mut latency = Vec::with_capacity(8192 * concurrency);
    let mut error_samples = Vec::new();
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !workers.is_empty() {
        tokio::select! {
            Some(result) = workers.join_next() => {
                let (count, cancelled_count, failed, values, diagnostics) = result.map_err(|error| Status::internal(error.to_string()))?;
                calls += count; cancelled += cancelled_count; errors += failed; latency.extend(values);
                error_samples.extend(diagnostics.into_iter().take(16 - error_samples.len()));
            },
            _ = interval.tick() => { if snapshots.len() < 4096 { snapshots.push(resource_snapshot()); } },
        }
    }
    let measured_secs = started.elapsed().as_secs_f64();
    let drained_at = tokio::time::Instant::now();
    while drained_at.elapsed() < Duration::from_secs(cooldown) {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if snapshots.len() < 4096 {
            snapshots.push(resource_snapshot());
        }
    }
    let after = resource_snapshot();
    latency.sort_unstable();
    let percentile = |percent: usize| {
        latency
            .get((latency.len().saturating_sub(1)) * percent / 100)
            .copied()
            .unwrap_or_default()
    };
    Ok(
        sonic_rs::json!({"schemaVersion":1,"event":"result","status":if errors==0 {"PASS"} else {"FAIL"},"scope":"local socket workload; resource samples include cooldown; no cross-protocol comparison","mode":mode,"duration_secs":measured_secs,"cooldown_secs":cooldown,"concurrency":concurrency,"payload_bytes":payload_bytes,"calls":calls,"cancelled":cancelled,"errors":errors,"error_samples":error_samples,"qps":calls as f64/measured_secs,"latency_us":{"p50":percentile(50),"p99":percentile(99)},"latency_sample_count":latency.len(),"baseline":baseline,"after":after,"samples":snapshots}),
    )
}

fn resource_snapshot() -> sonic_rs::Value {
    #[cfg(target_os = "linux")]
    let (fds, rss_kib) = {
        let fds = std::fs::read_dir("/proc/self/fd")
            .ok()
            .map(|files| files.count());
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let rss = status
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u64>().ok());
        (fds, rss)
    };
    #[cfg(not(target_os = "linux"))]
    let (fds, rss_kib): (Option<usize>, Option<u64>) = (None, None);
    sonic_rs::json!({"unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,"fds":fds,"rss_kib":rss_kib,"tasks":tokio::runtime::Handle::current().metrics().num_alive_tasks()})
}

impl PureUnary for Handler {
    async fn echo(
        &self,
        request: Request<PureUnaryEchoRequest>,
    ) -> Result<Response<Empty>, Status> {
        Ok(Response::new(request.into_inner().request))
    }
}

impl PureStreaming for Handler {
    async fn bidi(
        &self,
        request: Request<ThriftRecvStream<Empty>>,
    ) -> Result<Response<volo_grpc::thrift::SendStream<Empty>>, Status> {
        Ok(Response::new(volo_grpc::thrift::SendStream::from_stream(
            request.into_inner(),
        )))
    }
}

impl Parent for Handler {
    async fn inherited(&self, request: Request<Message>) -> Result<Response<ReplyStream>, Status> {
        Ok(Response::new(server_replies(request.into_inner())))
    }
}

/// Exercise the generated unary and streaming API using a single configured client.
pub async fn verify_client(client: &InteropClient) -> Result<Vec<&'static str>, Status> {
    let message = |value: &'static [u8]| Message {
        payload: Some(pilota::Bytes::from_static(value)),
        sequence: Some(7),
    };
    let mut passed = Vec::new();
    let first = client
        .unary(InteropUnaryRequest {
            request: message(b"raw\0payload"),
        })
        .await?;
    check_compression(first.metadata())?;
    assert_eq!(first.into_inner(), message(b"raw\0payload"));
    passed.push("unary-args-result");
    assert_eq!(
        client
            .explicit_unary(InteropExplicitUnaryRequest {
                request: message(b"explicit")
            })
            .await?
            .into_inner(),
        message(b"explicit")
    );
    passed.push("explicit-unary");
    client.zero(InteropZeroRequest {}).await?;
    passed.push("zero-void-stop");
    assert_eq!(
        client
            .multi(InteropMultiRequest {
                left: -3,
                right: i64::MAX
            })
            .await?
            .into_inner(),
        i64::MAX - 3
    );
    passed.push("multiple-parameters-field-ids");
    assert_eq!(
        client
            .container(InteropContainerRequest {
                values: vec![i64::MIN, 0, i64::MAX]
            })
            .await?
            .into_inner(),
        vec![i64::MIN, 0, i64::MAX]
    );
    passed.push("container-result");
    assert!(
        matches!(client.throw(InteropThrowRequest { request: message(b"failure") }).await?.into_inner(), Err(InteropThrowException::Failure(Failure { message })) if message.as_deref() == Some("declared failure"))
    );
    assert!(matches!(
        client
            .throw(InteropThrowRequest {
                request: message(b"other")
            })
            .await?
            .into_inner(),
        Err(InteropThrowException::Other(Other { code: Some(7) }))
    ));
    assert_eq!(
        client
            .throw(InteropThrowRequest {
                request: message(b"success")
            })
            .await?
            .into_inner(),
        Ok(message(b"success"))
    );
    passed.push("multiple-declared-exceptions");
    assert!(
        matches!(client.void_throw(InteropVoidThrowRequest {}).await?.into_inner(), Err(InteropVoidThrowException::Failure(Failure { message })) if message.as_deref() == Some("void failure"))
    );
    passed.push("single-declared-void-exception");
    let status = client
        .throw(InteropThrowRequest {
            request: message(b"biz"),
        })
        .await
        .expect_err("business error");
    let biz = volo_grpc::thrift::BizStatus::from_status(&status)?.expect("biz-status metadata");
    assert_eq!(biz.code, 409);
    assert_eq!(biz.message, "失败：\"\\\n");
    assert_eq!(biz.extra, [("中文\"\\".into(), "值\n\t\0".into())].into());
    passed.push("biz-status-unicode-json");
    for source in [
        vec![],
        vec![message(b"a")],
        vec![message(b"a"), message(b"bc")],
    ] {
        let count = source.len() as i64;
        let expected = source
            .iter()
            .flat_map(|value| value.payload.as_deref().unwrap_or_default().iter().copied())
            .collect::<Vec<_>>();
        let response = client.client(stream::iter(source)).await?.into_inner();
        assert_eq!(
            response.payload.as_deref().unwrap_or_default(),
            expected.as_slice()
        );
        assert_eq!(response.sequence, Some(count));
    }
    passed.push("client-stream-zero-one-many");
    for inherited in [false, true] {
        let mut response = if inherited {
            client.inherited(message(b"server")).await?.into_inner()
        } else {
            client.server(message(b"server")).await?.into_inner()
        };
        for sequence in 0..3 {
            assert_eq!(
                response.message().await?.unwrap(),
                Message {
                    payload: Some(pilota::Bytes::from_static(b"server")),
                    sequence: Some(sequence)
                }
            );
        }
        assert!(response.message().await?.is_none());
    }
    passed.push("server-stream-and-inherited-path");
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    // Kitex can delay response headers until its first Send. Make the first
    // request available while the transport is waiting for those headers.
    tx.send(message(b"before-half-close")).await.unwrap();
    let mut response = client
        .bidi(volo_grpc::codegen::ReceiverStream::new(rx))
        .await?
        .into_inner();
    assert_eq!(
        response.message().await?.unwrap(),
        message(b"before-half-close")
    );
    drop(tx);
    assert!(response.message().await?.is_none());
    passed.push("bidi-first-response-before-half-close");
    Ok(passed)
}
