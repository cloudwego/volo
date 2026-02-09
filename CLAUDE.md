# CLAUDE.md - Volo Workspace

For detailed per-crate documentation, see the CLAUDE.md in each sub-crate directory.

## Project Overview

[Volo](https://github.com/cloudwego/volo) is a high-performance Rust RPC framework by CloudWeGo (ByteDance). It supports Thrift, gRPC, and HTTP protocols with fully async design (Tokio), zero-copy optimizations, and middleware via Service/Layer abstractions (motore).

- Rust Edition: 2024
- MSRV: 1.85.0
- Current Version: 0.12.x

## Workspace Structure

```
volo/
├── volo/                   # Core library
├── volo-build/             # Code generation from IDL (Thrift/Protobuf)
├── volo-cli/               # CLI tool (project scaffolding)
├── volo-grpc/              # gRPC implementation
├── volo-http/              # HTTP implementation
├── volo-macros/            # Procedural macros (reserved)
├── volo-thrift/            # Thrift implementation
├── examples/               # Example code
├── benchmark/              # Performance benchmarks
└── tests/code-generation/  # Code generation tests
```

## Crate Dependency Graph

```
                     volo-macros (reserved)
                          |
                          v
    +------------------  volo  ------------------+
    |                     |                       |
    v                     v                       v
volo-thrift           volo-grpc              volo-http
    |                     |                       |
    +----------+----------+                       |
               v                                  |
          volo-build <----------------------------+
               |
               v
           volo-cli
```

## Crate Overview

- **volo**: Core abstractions -- service discovery (`Discover`), load balancing (`LoadBalance`), network transport (`Address`, `Conn`, TCP/Unix/TLS/ShmIPC), context (`RpcCx`, `RpcInfo`, `Endpoint`), hot restart, panic capture
- **volo-thrift**: TTHeader/Framed transport, Binary/Compact protocols, Ping-Pong/Multiplex modes, connection pooling, ISN-based multi-service routing, BizError
- **volo-grpc**: HTTP/2 (hyper), unary/streaming calls, compression (gzip/zlib/zstd), gRPC-Web, metadata
- **volo-http**: Server (Router/Handler/Extractor), Client (connection pooling/DNS/proxy), JSON/Form/Multipart/WebSocket/SSE, TLS (Rustls/Native-TLS)
- **volo-build**: Generates Rust code from Thrift/Protobuf IDL. Config: `volo.yml` / `volo.workspace.yml`
- **volo-cli**: `volo init`, `volo http init`, `volo idl add`, `volo repo add/update`, `volo migrate`
- **volo-macros**: Reserved. Active macros: `#[service]` (from motore), `volo_unreachable!`, `new_type!` (from volo)

## Feature Flags Summary

| Feature       | volo | volo-thrift | volo-grpc  | volo-http  |
| ------------- | ---- | ----------- | ---------- | ---------- |
| `rustls`      | Y    | -           | Y          | Y          |
| `native-tls`  | Y    | -           | Y          | Y          |
| `shmipc`      | Y    | Y           | -          | -          |
| `multiplex`   | -    | Y           | -          | -          |
| `gzip`/`zlib` | -    | -           | Y(default) | -          |
| `zstd`        | -    | -           | Y          | -          |
| `grpc-web`    | -    | -           | Y          | -          |
| `json`        | -    | -           | -          | Y(default) |
| `ws`          | -    | -           | -          | Y          |
| `cookie`      | -    | -           | -          | Y          |

## Core Abstractions

All built on the `motore` crate's `Service<Cx, Request>` and `Layer<S>` traits.

- **Service Discovery**: `Discover` trait (volo) -- `StaticDiscover`, `WeightedStaticDiscover`
- **Load Balancing**: `LoadBalance` trait (volo) -- weighted random, consistent hashing

## Design Patterns

- **Builder**: `XxxBuilder::new().xxx().build()`
- **Make**: `MakeXxx` trait creates `Xxx` instances
- **Layer**: `XxxLayer` implements `motore::layer::Layer`
- **Service**: Implements `motore::service::Service`
- **Private features**: Prefixed with `__` (e.g., `__tls`)

## Commit Conventions

Follow [Conventional Commits](https://www.conventionalcommits.org/): `feat(volo-thrift): add multi-service support`, `fix(volo-http): resolve connection pool leak`, `chore: update dependencies`

## Release Order

Publish in this order:

1. `volo-macros`
2. `volo`
3. `volo-build`
4. `volo-cli`
5. `volo-thrift`
6. `volo-grpc`
7. `volo-http` (released independently)
