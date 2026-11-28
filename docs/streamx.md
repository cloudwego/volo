# StreamX server bindings

`volo-streamx` serves typed Thrift and Protobuf bindings through one incoming.
Thrift unary and oneway use the existing ordinary Thrift codecs. Thrift streams
use TTStream or Thrift over gRPC; Protobuf uses gRPC. Each generated binding
shares one `Arc` of its handler across its protocol implementations.

Install `volo_streamx_build::plugin()` through the existing `volo_build` builder.
The generator belongs in build-dependencies; `volo-streamx` belongs in the
generated crate's dependencies. The runtime has no dependency on the build
plugin. See the [code generation fixture](../tests/streamx-codegen/build.rs) and
[handler and network examples](../tests/streamx-codegen/tests/bindings.rs).

For example, a build script can use the ordinary Thrift backend for all three
server transports and the Protobuf backend for gRPC:

```rust,ignore
fn main() {
    volo_build::Builder::thrift()
        .add_service("idl/chat.thrift")
        .plugin(volo_streamx_build::plugin())
        .filename("thrift.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::protobuf()
        .add_service("idl/presence.proto")
        .plugin(volo_streamx_build::plugin())
        .filename("protobuf.rs".into())
        .write()
        .unwrap();
}
```

Keep the normal dependencies required by the original generated messages and
clients. The [consumer manifest](../tests/streamx-codegen/Cargo.toml) contains
the complete runtime/build dependency split. A separate Thrift gRPC backend
invocation is needed only when generating that client's view; its existing
oneway restriction still applies.

Thrift `pilota.name` on a service or method changes its Rust identifier while
preserving the original wire name. Inherited Args/Result and exception types
use the defining service's Rust name. The [naming edge fixture](../tests/streamx-codegen/tests/name_edges.rs)
compiles both Thrift backends and exercises the same renamed handler through
ordinary Thrift, TTStream and gRPC. It also checks absent Thrift namespaces and
Protobuf packages against fixed-version Go service information.

```rust,ignore
volo_streamx::Server::new()
    .add_fallback_service(chat_service_streamx::bind(Chat))
    .add_service(archive_service_streamx::bind(Archive))
    .add_service(presence_service_streamx::bind(Presence))
    .graceful_shutdown_timeout(std::time::Duration::from_secs(5))
    .run("127.0.0.1:8080".parse::<std::net::SocketAddr>()?)
    .await?;
```

The fallback must cover conflicting method names across services, or enable
`refuse_traffic_without_service_name(true)` and require named ordinary Thrift
calls. Short service names are unique across both IDLs. All drivers query the
same frozen directory. gRPC uses a strict lookup without changing the ordinary
Thrift lookup policy.

`combine_service(name)` selects one registered Thrift service for non-strict
queries and requires that it be the only service. H2 and explicit strict
ordinary/TTStream queries still require its registered service name. Every
method binding comes from a finite IDL method table. Missing methods return the
native protocol's lookup error.

`prepare()` validates registration and native configuration without listening.
The returned `PreparedServer` exposes `handle()` for shutdown and connection
counters. `run` accepts a socket address, a Volo address, or a custom
`MakeIncoming`; `run_with_shutdown` also accepts a future returning
`io::Result<()>`. On Unix, `run` observes SIGINT, SIGHUP and SIGTERM.

A global `.layer(layer)` wraps each typed handler invocation once across all
protocols and RPC shapes. Layers compose statically using Motore `Stack`, with
`InvocationHandler` as the innermost service. Implement `Layer<S>` over a generic
inner service; the composed service implements
`Service<StreamContext, Invocation<'a>>` for every request lifetime, with
`Response = ()` and `Error = RpcError`. Layers run in registration
order, outermost first. Forward the borrowed invocation to the inner service;
returning an error skips business work. A successful layer must call its inner
service. The request and reply keep their original concrete types in the
invocation future. `prepare()` erases the complete stack once. With global layers
configured, each invocation allocates two futures: the business future, to bound
the scoped poll stack, and the erased stack future. Adding a layer introduces no
additional framework allocation. Calls without a global layer skip both allocations.
As with native server builders, `.layer()` changes the builder's generic type;
layers specialized to `InvocationService` should be generalized to `Layer<S>`.
Message send/recv paths retain their native ownership and backpressure.
Use the native configuration closures below for protocol-specific
request/response layers.

`.stream_hook(factory)` creates observers for native calls. A factory implements
`StreamHookFactory`, or is a closure `Fn(&StreamContext) -> H` with `H: StreamHook`.
Its optional `enabled` method can leave selected methods unobserved. Events carry
the common context, native DATA message sizes, metadata and native terminal
status. Metadata and half-close events identify send or receive direction.
Ordinary Thrift uses its existing unary business boundary for open/finish and
its original codec/tracer for wire statistics; it has no streaming DATA frames.
H2 unary has its native DATA events. Hook admission errors stop business; an
event callback panic disables that observer for the call. Other observers still
close normally. Finish waits for admitted callbacks, while native cancellation
and resource reclamation remain independent of observer completion.

For a managed H2 response, the driver commits the final status after accepting
its Trailer. If code, message and details still match the handler's candidate
result, the local status retains its error source, including transport phase
and whether bytes had been committed. Body layers may change the wire status;
their final fields take precedence. A Trailer write failure records the write
error instead. These local sources are not serialized to the peer.

A framework can call `cx.set_scope(Arc::new(scope))` in its hook factory. The
first `StreamScope` is retained by the native context and enters framework task
locals for handler/body polls, hooks and managed children. `enter` invokes its
operation exactly once and does not retain it. Existing protocol-specific scope
configuration remains available. See the `GlobalLayer`, `SharedObserver` and
`FrameworkScope` examples in the network fixture linked above.

Use `cx.spawn_scoped(work)` for work owned by the RPC, including downstream
calls that must exit when it ends. For an application-owned `tokio::spawn`,
capture the context and enter it explicitly on each poll:

```rust,ignore
let context = cx.clone();
let application_task = tokio::spawn(async move {
    context.scope(async {
        tokio::task::yield_now().await;
        assert_eq!(StreamContext::current().unwrap().id(), context.id());
    }).await;
});
application_task.await.unwrap();
```

Entering the scope supplies context; the application still owns that task's
cancellation and completion.

Metainfo uses task snapshots. Managed children inherit the caller's latest
snapshot when spawned; their updates do not implicitly modify the parent.
Return and apply results explicitly when application tasks need to exchange
metadata. The framework preserves TTStream admission metadata for its handler
and subsequent driver tasks; later handler changes stay isolated.
A gRPC response producer explicitly publishes backward transient changes before
the first response becomes ready and again at completion, for response headers
and trailers. Deletions are preserved, and changes made by an outer layer in the
request task take precedence over the producer's inherited values.

Use `.thrift(|server| ...)` to configure the native codec, layers, streaming
hooks and limits. Use `.grpc(|server| ...)` for native H2 layers, windows,
keepalive and TLS. Service registration belongs to the StreamX builder. The
native builders are prepared once and their connection drivers retain their
existing protocol and RPC lifecycle state.

Configure a generated binding's H2 codec before registration with
`binding.configure_grpc(|service| ...)`. The closure receives the native
`volo_grpc::server::ServiceBuilder` and can set `codec_limits`, send/accept
compression, typed service layers and native stream hooks. Return the builder;
the binding builds and retains that route once. Cloning the binding preserves
the configuration and the same handler `Arc`. Other services and the binding's
ordinary Thrift/TTStream paths keep their own configuration.

```rust,ignore
let chat = chat_service_streamx::bind(Chat).configure_grpc(|service| {
    use volo_grpc::codec::compression::CompressionEncoding;
    service
        .codec_limits(volo_grpc::codec::CodecLimits::default())
        .send_compressions(vec![CompressionEncoding::Gzip(None)])
        .accept_compressions(vec![CompressionEncoding::Gzip(None)])
});
volo_streamx::Server::new().add_fallback_service(chat);
```

`binding.grpc()` returns the raw typed service for an explicit gRPC server;
its caller builds that server's native codec. `configure_grpc` sets the route
used by StreamX registration.

For multiple enabled drivers, the connection task reads eight application
bytes once and replays them to the selected driver. HTTP/2 keeps the complete
duplex connection; a Thrift driver receives the original read and write halves
with the prefix already buffered. The classified Thrift paths never call
`probe_ttstream`. Selecting one driver with `protocols(Protocols::GRPC)` or the
other protocol constants bypasses detection. A connection stays on its selected
driver for its lifetime.

HTTP/1 configuration is rejected before creating the incoming, including on
single-driver entries. StreamX H2 validates gRPC content-type for both payloads;
the original gRPC server retains its existing policy. Two payloads can run on
the same physical H2 connection.

The `rustls` or `native-tls` feature enables `.tls_config(...)`. One TCP TLS
acceptor serves every enabled driver. No ALPN uses normal detection; negotiated
`h2` requires H2 traffic. Configured ALPN advertisements may contain only `h2`.
An incoming that supplies an already negotiated TLS connection retains that
session and its peer information. Known non-TCP incoming factories reject TCP
TLS before binding. A custom factory can implement `MakeIncoming::supports_tls`
to declare its capability; otherwise the accepted connection is checked before
reading application bytes. The `shmipc` feature compiles the native handoff and
helper paths; ShmIPC runtime interoperability is tracked separately.

Shutdown stops admission, requests native H2 and TTStream drain, and waits for
accepted calls and framework-managed tasks under one deadline. Accepted ordinary
Thrift unary and oneway keep running after peer EOF or disconnect. At the server
deadline, native contexts receive `CancelCause::ServerShutdown` before their
owning futures are aborted. TTStream uses this shared deadline instead of its
explicit server entry's independent drain timer. Dropping the run future also
publishes server cancellation and aborts owned tasks. Arbitrary application
`tokio::spawn` tasks and blocking code retain application ownership.

`connection_observer` receives one selected event and one closed event per
classified connection, and a bounded rejection reason for rejected inputs.
Events use enums for protocol and transport. The callback runs synchronously;
panics are contained so connection cleanup can finish. `ServerHandle::stats`
provides live connection, native call and managed task counts.

`StreamContext::payload()` exposes the message format independently of the
transport. `route()` returns a borrowed `RpcRoute`
with the resolved service, method, payload and mode for a registered server
method. Registered raw TTStream methods can have `mode: None`. Client calls and
unresolved calls return no registered route. These accessors use the existing
native call state and directory; they create no new registry or event task.

An alias path resolves to the canonical typed route; `OriginalGrpcPath` retains
the received path for diagnostics only when that rewrite changes the path.
For an already canonical path, the native RPC context's method is the received
path and no duplicate extension is stored. A custom metrics exporter
should use `route()` and map `None` to a fixed label such as `unknown`. Raw paths,
rejection text, peer
ports and metadata belong in bounded logs/traces, not metric dimensions.
StreamX's built-in connection observations and counters only use enums and
fixed fields; it does not install a metrics exporter itself.

Generate Thrift streaming consumers and implement the common
`volo::streaming` signatures returning
`RpcResult`; explicit TTStream and Thrift gRPC server bindings use those same
types. Existing published ordinary Thrift and Protobuf gRPC traits remain
available. The plugin adds direct typed dispatch and descriptors without
duplicating message types.

Use the same explicit protocol clients against a candidate before replacing
instances. Protocol examples and regression tests are in
[streamx-codegen](../tests/streamx-codegen/tests). A rollback process on the same address must still
support every protocol sent to that address. To restore separate native Rust
servers, restore the corresponding addresses/client routing first and drain
the mixed listener; changing it directly into a single-protocol listener would
remove the other transports. Keep the same IDL wire names and metadata rules.
