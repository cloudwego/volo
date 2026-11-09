//! The same public Protobuf workload builds against the baseline and candidate.
use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures::{TryStreamExt, stream};
use http_body::Frame;
use motore::Service;
use volo::{
    context::{Endpoint, Role, RpcInfo},
    net::{Address, incoming::DefaultIncoming},
};
use volo_grpc::{
    RecvEntryMessage, RecvStream, Request, Response, SendEntryMessage, Status,
    body::{Body, BoxBody, boxed},
    client::Http2Config,
    codec::{compression::CompressionEncoding, decode::Kind},
    context::{ClientContext, Config, ServerContext},
    server::{NamedService, Server},
    transport::ClientTransport,
};

// Mirror a generated message with one `bytes` field. Pilota's well-known Bytes
// wrapper has a pre-existing zero-copy length-accounting bug for large values;
// measuring that wrapper would fail in both baseline and candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Payload(Bytes);
impl pilota::pb::Message for Payload {
    fn encode_raw(&self, output: &mut pilota::LinkedBytes) {
        pilota::pb::encoding::bytes::encode(1, &self.0, output);
    }
    fn merge_field(
        &mut self,
        tag: u32,
        wire: pilota::pb::encoding::WireType,
        input: &mut Bytes,
        context: &mut pilota::pb::encoding::DecodeContext,
        _: bool,
    ) -> Result<(), pilota::pb::DecodeError> {
        if tag == 1 {
            pilota::pb::encoding::bytes::merge(wire, &mut self.0, input, context)
        } else {
            pilota::pb::encoding::skip_field(wire, tag, input, context)
        }
    }
    fn encoded_len(&self, context: &mut pilota::pb::encoding::EncodeLengthContext) -> usize {
        pilota::pb::encoding::bytes::encoded_len(context, 1, &self.0)
    }
}

struct Outbound(Payload);
impl SendEntryMessage for Outbound {
    fn into_body(
        self,
        compression: Option<CompressionEncoding>,
    ) -> volo_grpc::BoxStream<'static, Result<Frame<Bytes>, Status>> {
        volo_grpc::codec::encode::encode(stream::iter([Ok(self.0)]), compression)
    }
}
struct Reply(RecvStream<Payload>);
impl RecvEntryMessage for Reply {
    fn from_body(
        _: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression: Option<CompressionEncoding>,
    ) -> Result<Self, Status> {
        Ok(Self(RecvStream::new(body, kind, compression)))
    }
}
#[derive(Clone, Copy)]
struct Echo;
impl NamedService for Echo {
    const NAME: &'static str = "bench.Echo";
}
impl Service<ServerContext, Request<BoxBody>> for Echo {
    type Response = Response<BoxBody>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        request: Request<BoxBody>,
    ) -> Result<Self::Response, Status> {
        let mut request = RecvStream::<Payload>::new(request.into_inner(), Kind::Request, None);
        let value = request
            .try_next()
            .await?
            .ok_or_else(|| Status::internal("missing request"))?;
        if request.try_next().await?.is_some() {
            return Err(Status::internal("extra request"));
        }
        Ok(Response::new(boxed(Body::new(
            volo_grpc::codec::encode::encode(stream::iter([Ok(value)]), None),
        ))))
    }
}
async fn exchange(
    transport: &ClientTransport<Reply>,
    address: Address,
    payload: Payload,
) -> Result<(), Status> {
    let mut callee = Endpoint::new("bench-peer".into());
    callee.set_address(address);
    let mut cx = ClientContext::new(RpcInfo::new(
        Role::Client,
        "/bench.Echo/Call".into(),
        Endpoint::new("bench-caller".into()),
        callee,
        Config::default(),
    ));
    let mut response = transport
        .call(&mut cx, Request::new(Outbound(payload.clone())))
        .await?
        .into_inner()
        .0;
    if response.try_next().await? != Some(payload) {
        return Err(Status::internal("echo differs"));
    }
    if response.try_next().await?.is_some() {
        return Err(Status::internal("extra response"));
    }
    Ok(())
}
fn option(args: &[String], name: &str, default: &str) -> String {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| default.into())
}

#[tokio::main(worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = std::env::args().collect::<Vec<_>>();
    let duration: u64 = option(&args, "--seconds", "3").parse()?;
    let concurrency: usize = option(&args, "--concurrency", "32").parse()?;
    let size: usize = option(&args, "--payload-bytes", "1024").parse()?;
    let selected = option(&args, "--transport", "tcp");
    if !(1..=120).contains(&duration) || !(1..=256).contains(&concurrency) || size > 1024 * 1024 {
        return Err("benchmark parameter limit".into());
    }
    let sockets = tempfile::Builder::new().prefix("grpc-bench-").tempdir()?;
    let (incoming, address) = if selected == "tcp" {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?.into();
        (DefaultIncoming::from(listener), address)
    } else if selected == "uds" {
        #[cfg(unix)]
        {
            let path = sockets.path().join("rpc.sock");
            let listener = tokio::net::UnixListener::bind(&path)?;
            (
                DefaultIncoming::from(listener),
                Address::Unix(std::os::unix::net::SocketAddr::from_pathname(path)?),
            )
        }
        #[cfg(not(unix))]
        {
            return Err("UDS is unavailable on this platform".into());
        }
    } else {
        return Err("benchmark supports tcp/uds".into());
    };
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut server = tokio::spawn(async move {
        Server::new()
            .add_service(Echo)
            .run_with_shutdown(incoming, async {
                let _ = stopped.await;
                Ok(())
            })
            .await
    });
    let transport = Arc::new(ClientTransport::<Reply>::new(
        &Http2Config::default(),
        &Config::default(),
    ));
    let payload = Payload(Bytes::from(vec![b'x'; size]));
    let probe = exchange(&transport, address.clone(), payload.clone());
    let rpc_future_bytes = std::mem::size_of_val(&probe);
    drop(probe);
    for _ in 0..32 {
        exchange(&transport, address.clone(), payload.clone()).await?;
    }
    let begin = tokio::time::Instant::now();
    let until = begin + Duration::from_secs(duration);
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..concurrency {
        let (transport, address, payload) = (transport.clone(), address.clone(), payload.clone());
        workers.spawn(async move {
            let (mut count, mut errors) = (0u64, 0u64);
            let mut latencies = Vec::with_capacity(8192);
            let mut first_error = None;
            while tokio::time::Instant::now() < until {
                let tick = tokio::time::Instant::now();
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    exchange(&transport, address.clone(), payload.clone()),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        errors += 1;
                        first_error.get_or_insert_with(|| error.to_string());
                    }
                    Err(error) => {
                        errors += 1;
                        first_error.get_or_insert_with(|| error.to_string());
                    }
                }
                let latency = tick.elapsed().as_micros() as u64;
                if latencies.len() < 8192 {
                    latencies.push(latency);
                } else {
                    latencies[count as usize % 8192] = latency;
                }
                count += 1;
            }
            (count, errors, latencies, first_error)
        });
    }
    let (mut calls, mut errors) = (0u64, 0u64);
    let mut latencies = Vec::new();
    let mut first_error = None;
    while let Some(result) = workers.join_next().await {
        let (n, e, samples, error) = result?;
        calls += n;
        errors += e;
        latencies.extend(samples);
        if first_error.is_none() {
            first_error = error;
        }
    }
    let elapsed = begin.elapsed().as_secs_f64();
    drop(transport);
    let _ = stop.send(());
    let shutdown_finished = tokio::time::timeout(Duration::from_secs(2), &mut server)
        .await
        .is_ok();
    if !shutdown_finished {
        server.abort();
        let _ = server.await;
    }
    latencies.sort_unstable();
    let quantile = |percent| {
        latencies
            .get((latencies.len().saturating_sub(1)) * percent / 100)
            .copied()
            .unwrap_or_default()
    };
    println!(
        "{}",
        serde_json::json!({"schemaVersion":1,"status":if errors==0 {"PASS"} else {"FAIL"},"protocol":"grpc-protobuf","scope":"same-process socket unary, no hook/deadline, identity compression","transport":selected,"concurrency":concurrency,"payload_bytes":size,"seconds":elapsed,"calls":calls,"errors":errors,"first_error":first_error,"qps":calls as f64/elapsed,"latency_us":{"p50":quantile(50usize),"p99":quantile(99usize)},"latency_sample":"latest 8192 per worker","rpc_future_bytes":rpc_future_bytes,"shutdown_finished":shutdown_finished})
    );
    if errors != 0 {
        return Err("benchmark RPC failure".into());
    }
    Ok(())
}
