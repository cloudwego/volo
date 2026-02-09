# CLAUDE.md - Volo Core Crate

## Overview

`volo` is the core foundation library providing shared abstractions (service discovery, load balancing, network transport, context management) used by `volo-thrift`, `volo-grpc`, and `volo-http`.

## Directory Structure

```
volo/src/
├── lib.rs              # Library entry, exports public API
├── client.rs           # Client service trait definitions (ClientService, OneShotService, MkClient)
├── context.rs          # RPC context and metadata (RpcCx, RpcInfo, Endpoint, Role)
├── hack.rs             # Unsafe optimization tools (conditional compilation)
├── macros.rs           # Utility macro definitions
│
├── catch_panic/        # Panic capture layer for services
├── discovery/          # Service discovery (Discover trait, Instance, StaticDiscover)
├── hotrestart/         # Hot restart support (Unix only)
│
├── loadbalance/        # Load balancing
│   ├── mod.rs          # LoadBalance trait, LbConfig
│   ├── layer.rs        # LoadBalanceLayer (motore Layer)
│   ├── error.rs        # LoadBalanceError (Retry, Discover, MissRequestHash)
│   ├── random.rs       # WeightedRandomBalance
│   └── consistent_hash.rs  # ConsistentHashBalance (requires RequestHash)
│
├── net/                # Network transport layer
│   ├── mod.rs          # Address enum (Ip, Unix, Shmipc)
│   ├── conn.rs         # ConnStream, Conn, OwnedReadHalf/OwnedWriteHalf
│   ├── dial.rs         # Client connection establishment (MakeTransport)
│   ├── incoming.rs     # Server connection acceptance (MakeIncoming, Incoming)
│   ├── ext.rs          # AsyncExt trait (check IO ready state)
│   ├── probe.rs        # IPv4/IPv6 network probing
│   ├── tls/            # TLS support (TlsConnector, TlsAcceptor, ClientTlsConfig, ServerTlsConfig)
│   └── shmipc/         # Shared memory IPC transport (optional)
│
└── util/
    ├── mod.rs          # Ref<'a, B> - borrowed reference or Arc
    ├── buf_reader.rs   # BufReader with compact() and fill_buf_at_least()
    └── remote_error.rs # Remote connection error detection
```

## Key Modules

### Service Discovery (`discovery`)

`Discover` trait for resolving service endpoints to instances. Built-in implementations: `StaticDiscover`, `WeightedStaticDiscover`, `DummyDiscover`.

### Load Balancing (`loadbalance`)

`LoadBalance` trait for selecting instances. Strategies: `WeightedRandomBalance`, `ConsistentHashBalance`. Applied via `LoadBalanceLayer`.

### Context (`context`)

`RpcCx<I, Config>` wraps `RpcInfo` (role, method, caller/callee endpoints). `newtype_impl_context!` macro implements the `Context` trait for newtypes.

### Network (`net`)

Unified transport abstraction. `Address` enum supports TCP (`Ip`), Unix sockets (`Unix`), and shared memory (`Shmipc`). `ConnStream` enum wraps all connection types.

### Hot Restart (`hotrestart`, Unix only)

Zero-downtime restarts via Unix Domain Socket. Parent passes listening socket FDs to child process via `SCM_RIGHTS`, then child signals parent to terminate. Global instance: `DEFAULT_HOT_RESTART`.

### Panic Capture (`catch_panic`)

Layer that catches panics in service calls. `Handler` trait defines custom panic handling. `PanicInfo` provides message, location, and stack trace.

## Important Notes

- **`VOLO_ENABLE_REMOTE_CLOSED_ERROR_LOG`**: Environment variable that controls whether remote connection closed errors are logged (see `util/remote_error.rs`).
- **`volo_unreachable!()`**: Macro that becomes `unreachable_unchecked()` when the `unsafe_unchecked` feature is enabled; otherwise a normal `unreachable!()`.
- **`new_type!`**: Macro for defining newtype wrappers with common trait implementations.
- **`volo::spawn()`**: Spawns a tokio task that automatically derives `metainfo` context.

## Feature Flags

| Feature               | Description                                               |
| --------------------- | --------------------------------------------------------- |
| `unsafe_unchecked`    | Use `unwrap_unchecked` instead of `unwrap` (optimization) |
| `tls` / `rustls`      | Equivalent to `rustls-aws-lc-rs`                          |
| `rustls-aws-lc-rs`    | Rustls with AWS LC crypto backend                         |
| `rustls-ring`         | Rustls with Ring crypto backend                           |
| `native-tls`          | System native TLS (OpenSSL/Secure Transport/SChannel)     |
| `native-tls-vendored` | Use vendored OpenSSL                                      |
| `shmipc`              | Enable shared memory IPC transport                        |

No default features are enabled.
