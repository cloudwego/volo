//! I10 server. Stdin requests snapshots; SIGINT uses every native server's shutdown.
use std::{
    io::{self, BufRead},
    sync::Arc,
    time::Duration,
};

use serde_json::json;
use volo::net::incoming::DefaultIncoming;
use volo_grpc::codec::compression::CompressionEncoding;

use crate::{
    grpc::chat as grpc,
    perf_runtime,
    perf_server::{CountIncoming, Counts, Echo},
    protobuf::streamx::presence as pb,
    thrift::chat,
};

#[cfg(feature = "perf-alloc")]
#[path = "perf_allocation.rs"]
mod allocation;
#[cfg(feature = "perf-alloc")]
#[global_allocator]
static ALLOCATOR: allocation::Allocator = allocation::Allocator;

fn snapshot(counts: &Counts, handle: Option<&volo_streamx::ServerHandle>) -> serde_json::Value {
    #[cfg(feature = "perf-alloc")]
    let counters = allocation::snapshot();
    let mut result = counts.snapshot(handle);
    perf_runtime::observe_process_resources(&mut result);
    #[cfg(feature = "perf-alloc")]
    {
        result["allocations"] = counters;
    }
    result
}

fn arg(name: &str, default: &str) -> String {
    std::env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|v| v[0] == name)
        .map(|v| v[1].clone())
        .unwrap_or_else(|| default.into())
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let entry = arg("--entry", "mixed");
    let protocol = arg("--protocol", "grpc-protobuf");
    let compression = arg("--compression", "identity");
    if !matches!(entry.as_str(), "mixed" | "explicit")
        || !matches!(
            protocol.as_str(),
            "thrift" | "ttstream" | "grpc-thrift" | "grpc-protobuf"
        )
        || !matches!(compression.as_str(), "identity" | "gzip")
        || (compression == "gzip" && !protocol.starts_with("grpc-"))
    {
        return Err(io::Error::other("invalid entry/protocol/compression").into());
    }
    let target = arg("--addr", "tcp://127.0.0.1:0");
    let (address, inner) = if let Some(addr) = target.strip_prefix("tcp://") {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        (
            format!("tcp://{}", listener.local_addr()?),
            DefaultIncoming::from(listener),
        )
    } else if let Some(path) = target.strip_prefix("unix://") {
        #[cfg(unix)]
        {
            (
                target.clone(),
                DefaultIncoming::from(tokio::net::UnixListener::bind(path)?),
            )
        }
        #[cfg(not(unix))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unix socket {path:?} is not supported on this platform"),
        )
        .into());
    } else {
        return Err(io::Error::other("expected tcp:// or unix://").into());
    };
    let counts = Arc::new(Counts::default());
    let incoming = CountIncoming {
        inner,
        counts: counts.clone(),
    };
    let encodings = if compression == "gzip" {
        vec![CompressionEncoding::Gzip(None)]
    } else {
        vec![]
    };
    let thrift = chat::chat_service_streamx::bind(Echo(counts.clone())).configure_grpc(|s| {
        s.send_compressions(encodings.clone())
            .accept_compressions(encodings.clone())
    });
    let proto = pb::presence_service_streamx::bind(Echo(counts.clone())).configure_grpc(|s| {
        s.send_compressions(encodings.clone())
            .accept_compressions(encodings.clone())
    });
    let (mut server, handle): (futures::future::BoxFuture<'static, _>, _) = if entry == "mixed" {
        let server = perf_runtime::server()
            .add_fallback_service(thrift)
            .add_service(proto)
            .graceful_shutdown_timeout(Duration::from_secs(2))
            .prepare()?;
        let handle = server.handle();
        (
            Box::pin(perf_runtime::run_mixed!(server, incoming)),
            Some(handle),
        )
    } else if protocol == "grpc-protobuf" {
        let leaf = volo_grpc::server::ServiceBuilder::new(pb::PresenceServiceServer::new(Echo(
            counts.clone(),
        )))
        .send_compressions(encodings.clone())
        .accept_compressions(encodings)
        .build();
        (
            Box::pin(perf_runtime::run_grpc!(
                volo_grpc::server::Server::new().add_service(leaf),
                incoming,
            )),
            None,
        )
    } else if protocol == "grpc-thrift" {
        let leaf = volo_grpc::server::ServiceBuilder::new(grpc::ChatServiceServer::new(Echo(
            counts.clone(),
        )))
        .send_compressions(encodings.clone())
        .accept_compressions(encodings)
        .build();
        (
            Box::pin(perf_runtime::run_grpc!(
                volo_grpc::server::Server::new().add_service(leaf),
                incoming,
            )),
            None,
        )
    } else {
        (
            Box::pin(perf_runtime::run_thrift!(
                chat::ChatServiceServer::new(Echo(counts.clone())),
                incoming
            )),
            None,
        )
    };
    // The listener already exists and CountIncoming::make_incoming is ready.
    // Enter native/SDK startup before announcing readiness, so the explicit
    // Thrift accept task is present in the first resource snapshot.
    if let std::task::Poll::Ready(result) = futures::poll!(&mut server) {
        result?;
        return Err(io::Error::other("server ended before readiness").into());
    }
    let (send, mut recv) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let observed_counts = counts.clone();
    let observed_handle = handle.clone();
    let observer = tokio::spawn(async move {
        while let Some(id) = recv.recv().await {
            #[cfg(feature = "perf-alloc")]
            if id == "allocation-start" {
                allocation::reset_peak();
            }
            println!(
                "{}",
                json!({"event":"snapshot","id":id,"stats":snapshot(&observed_counts,observed_handle.as_ref())})
            );
        }
    });
    println!(
        "{}",
        json!({"event":"ready","addr":address,"entry":entry,"protocol":protocol,"compression":compression,"worker_threads":4,"endpoint_profile":perf_runtime::PROFILE,"allocation_instrumented":cfg!(feature="perf-alloc")})
    );
    server.await?;
    observer.abort();
    let _ = observer.await;
    println!(
        "{}",
        json!({"event":"stopped","stats":snapshot(&counts,handle.as_ref())})
    );
    Ok(())
}
