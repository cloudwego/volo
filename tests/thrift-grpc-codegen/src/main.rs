use std::time::Duration;

use thrift_grpc_codegen::{
    fixture::{Handler, verify_client},
    volo_gen::grpc_thrift::*,
};
use volo::net::{Address, incoming::DefaultIncoming};
use volo_grpc::server::{Server, ServiceBuilder};

fn option(args: &[String], key: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| pair[1].clone())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = std::env::args().collect::<Vec<_>>();
    let role = option(&args, "--role").ok_or("missing --role")?;
    let compression = option(&args, "--compression").unwrap_or_else(|| "identity".into());
    if compression != "identity" && compression != "gzip" {
        return Err("invalid compression".into());
    }
    let compressions = if compression == "gzip" {
        vec![volo_grpc::codec::compression::CompressionEncoding::Gzip(
            None,
        )]
    } else {
        Vec::new()
    };
    thrift_grpc_codegen::fixture::expect_compression(compression == "gzip");
    let address = option(&args, "--addr").unwrap_or_else(|| "tcp://127.0.0.1:0".into());
    let addr = parse_address(&address)?;
    if let Some(forward) = option(&args, "--forward") {
        thrift_grpc_codegen::fixture::forward_to(
            InteropClientBuilder::new("interop")
                .address(parse_address(&forward)?)
                .send_compressions(compressions.clone())
                .accept_compressions(compressions.clone())
                .build(),
        );
    }
    match role.as_str() {
        "grpc-thrift-server" => {
            let max_concurrent_streams = option(&args, "--server-max-streams")
                .map(|value| value.parse::<u32>())
                .transpose()?;
            let (incoming, bound) = match addr {
                Address::Ip(address) => {
                    let listener = tokio::net::TcpListener::bind(address).await?;
                    let bound = format!("tcp://{}", listener.local_addr()?);
                    (DefaultIncoming::from(listener), bound)
                }
                #[cfg(unix)]
                Address::Unix(address) => (
                    DefaultIncoming::from(tokio::net::UnixListener::bind(
                        address.as_pathname().ok_or("UDS pathname required")?,
                    )?),
                    address_string(&address),
                ),
                #[allow(unreachable_patterns)]
                _ => return Err("unsupported fixture transport".into()),
            };
            println!(
                "{}",
                sonic_rs::json!({"event":"ready", "addr":bound, "protocol":"grpc-thrift", "implementation":"volo", "max_concurrent_streams":max_concurrent_streams})
            );
            Server::new()
                .http2_max_concurrent_streams(max_concurrent_streams)
                .add_service(
                    ServiceBuilder::new(InteropServer::new(Handler))
                        .send_compressions(compressions.clone())
                        .accept_compressions(compressions.clone())
                        .build(),
                )
                .add_service(
                    ServiceBuilder::new(PureUnaryServer::new(Handler))
                        .send_compressions(compressions.clone())
                        .accept_compressions(compressions.clone())
                        .build(),
                )
                .add_service(
                    ServiceBuilder::new(PureStreamingServer::new(
                        thrift_grpc_codegen::metadata_values::MetadataValuesHandler,
                    ))
                    .send_compressions(compressions.clone())
                    .accept_compressions(compressions.clone())
                    .build(),
                )
                .run_with_shutdown(incoming, async { tokio::signal::ctrl_c().await })
                .await?;
        }
        "grpc-thrift-metadata-values-client" => {
            let client = PureStreamingClientBuilder::new("interop")
                .address(addr)
                .build();
            thrift_grpc_codegen::metadata_values::verify(&client).await?;
            let report = sonic_rs::json!({"schemaVersion":1,"event":"result","status":"PASS","cases":["ascii-multi-binary-header-trailer"]});
            if let Some(path) = option(&args, "--report") {
                std::fs::write(path, sonic_rs::to_string_pretty(&report)?)?;
            }
            println!("{report}");
        }
        "grpc-thrift-client" => {
            let client = InteropClientBuilder::new("interop")
                .address(addr)
                .send_compressions(compressions.clone())
                .accept_compressions(compressions.clone())
                .build();
            let passed =
                tokio::time::timeout(Duration::from_secs(15), verify_client(&client)).await??;
            let report = sonic_rs::to_string_pretty(
                &sonic_rs::json!({"schemaVersion":1,"event":"result","status":"PASS","protocol":"grpc-thrift","cases":passed,"implementation":"volo"}),
            )?;
            if let Some(path) = option(&args, "--report") {
                std::fs::write(path, &report)?;
            }
            println!("{report}");
        }
        "grpc-thrift-lifecycle-client" => {
            let scenario = option(&args, "--scenario").ok_or("missing --scenario")?;
            let id = option(&args, "--id").ok_or("missing --id")?;
            let client = InteropClientBuilder::new("interop")
                .address(addr)
                .send_compressions(compressions.clone())
                .accept_compressions(compressions.clone())
                .rpc_timeout(
                    scenario
                        .starts_with("deadline-")
                        .then_some(Duration::from_millis(1000)),
                )
                .build();
            let mut info = metainfo::MetaInfo::default();
            {
                use metainfo::Forward;
                info.set_transient("INTEROP_ONCE", "seed");
                info.set_persistent("INTEROP_PERSIST", "seed");
            }
            let code = metainfo::METAINFO
                .scope(
                    std::cell::RefCell::new(info),
                    thrift_grpc_codegen::fixture::lifecycle_client(&client, &scenario, &id),
                )
                .await?;
            let report = sonic_rs::json!({"schemaVersion":1,"event":"result","status":"PASS","scenario":scenario,"id":id,"code":code as i32});
            if let Some(path) = option(&args, "--report") {
                std::fs::write(path, sonic_rs::to_string_pretty(&report)?)?;
            }
            println!("{report}");
        }
        "grpc-thrift-response-client" => {
            let mode = option(&args, "--mode").unwrap_or_else(|| "unary".into());
            let client = InteropClientBuilder::new("interop")
                .address(addr)
                .send_compressions(compressions.clone())
                .accept_compressions(compressions.clone())
                .accept_compressions(vec![
                    volo_grpc::codec::compression::CompressionEncoding::Gzip(None),
                ])
                .build();
            let report = thrift_grpc_codegen::fixture::response_probe(&client, &mode).await;
            if let Some(path) = option(&args, "--report") {
                std::fs::write(path, sonic_rs::to_string_pretty(&report)?)?;
            }
            println!("{report}");
        }
        "grpc-thrift-keepalive-client" => {
            let scenario = option(&args, "--scenario").unwrap_or_else(|| "valid".into());
            if scenario != "valid" && scenario != "excessive" {
                return Err("invalid keepalive scenario".into());
            }
            let excessive = scenario == "excessive";
            let client = InteropClientBuilder::new("interop")
                .address(addr)
                .http2_adaptive_window(false)
                .http2_keepalive_interval(Duration::from_millis(if excessive { 20 } else { 100 }))
                .http2_keepalive_timeout(Duration::from_millis(500))
                .http2_keepalive_while_idle(false)
                .build();
            let report = thrift_grpc_codegen::fixture::verify_keepalive(&client, excessive).await?;
            if let Some(path) = option(&args, "--report") {
                std::fs::write(path, sonic_rs::to_string_pretty(&report)?)?;
            }
            println!("{report}");
        }
        "grpc-thrift-load-client" | "grpc-thrift-benchmark-client" => {
            let duration = option(&args, "--duration-secs")
                .unwrap_or_else(|| "10".into())
                .parse::<u64>()?;
            let concurrency = option(&args, "--concurrency")
                .unwrap_or_else(|| "16".into())
                .parse::<usize>()?;
            let payload = option(&args, "--payload-bytes")
                .unwrap_or_else(|| "128".into())
                .parse::<usize>()?;
            let cooldown = option(&args, "--cooldown-secs")
                .unwrap_or_else(|| "0".into())
                .parse::<u64>()?;
            let builder = InteropClientBuilder::new("interop")
                .address(addr)
                .send_compressions(compressions.clone())
                .accept_compressions(compressions.clone());
            let hooks = option(&args, "--hooks").unwrap_or_else(|| "off".into());
            let builder = match hooks.as_str() {
                "off" => builder,
                "on" => builder.stream_hook(|_: &volo_grpc::streaming::StreamContext| {
                    volo_grpc::streaming::NoopHooks
                }),
                _ => return Err("invalid hooks option".into()),
            };
            let phases = match option(&args, "--phase-latency").as_deref().unwrap_or("off") {
                "off" => None,
                "on" => Some(thrift_grpc_codegen::phase_latency::PhaseRecorder::new(32)),
                _ => return Err("invalid phase-latency option".into()),
            };
            let builder = match &phases {
                Some(recorder) => builder.stream_hook(recorder.clone()),
                None => builder,
            };
            let client = builder.build();
            println!(
                "{}",
                sonic_rs::json!({"event":"load-started","duration_secs":duration})
            );
            let mut report = if role == "grpc-thrift-benchmark-client" {
                let mode = match option(&args, "--mode").as_deref().unwrap_or("bidi") {
                    "unary" => "unary",
                    "client" => "client",
                    "server" => "server",
                    "bidi" => "bidi",
                    _ => return Err("invalid benchmark mode".into()),
                };
                thrift_grpc_codegen::fixture::run_benchmark(
                    client,
                    Duration::from_secs(duration),
                    concurrency,
                    payload,
                    mode,
                )
                .await?
            } else {
                thrift_grpc_codegen::fixture::run_load(
                    client,
                    Duration::from_secs(duration),
                    concurrency,
                    payload,
                    cooldown,
                )
                .await?
            };
            if let Some(recorder) = phases {
                report["phase_latencies"] = recorder.summary();
            }
            if let Some(path) = option(&args, "--report") {
                std::fs::write(path, sonic_rs::to_string_pretty(&report)?)?;
            }
            println!("{report}");
        }
        _ => return Err(format!("unsupported fixture role {role}").into()),
    }
    Ok(())
}

#[cfg(unix)]
fn address_string(address: &std::os::unix::net::SocketAddr) -> String {
    format!("unix://{}", address.as_pathname().unwrap().display())
}

fn parse_address(address: &str) -> Result<Address, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(address) = address.strip_prefix("tcp://") {
        return Ok(Address::Ip(address.parse()?));
    }
    if let Some(path) = address.strip_prefix("unix://") {
        #[cfg(unix)]
        {
            return Ok(Address::Unix(
                std::os::unix::net::SocketAddr::from_pathname(path)?,
            ));
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            return Err("UDS is unavailable on this target".into());
        }
    }
    Err("address must start tcp:// or unix://".into())
}
