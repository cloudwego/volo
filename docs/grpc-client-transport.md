# volo-grpc Client Transport: `:authority` and Connection Pooling

## 1. Overview

The volo-grpc client transport (`volo-grpc/src/transport/`) turns a call that service discovery
and load balancing have already routed to an `Address` into an HTTP/2 request. Two things about
it are worth knowing as a user:

- **Which `:authority` the server sees.** It is derived from what the client was configured
  with — the TLS server name or the service name — not from the socket that happens to be dialed.
  A proxy or ingress that routes on virtual host therefore sees the same host name the client
  presented for SNI, with no extra configuration.
- **How connections are kept.** One multiplexed HTTP/2 connection per callee address, held in the
  generic `volo::pool::Pool` from the core crate, with idle eviction.

## 2. `:authority` selection

HTTP/2 has no `Host` header; the request's authority is carried in the `:authority`
pseudo-header, which hyper takes from the request URI. volo-grpc builds that URI per call from
the callee `Endpoint` in the context, using the first rule that applies:

| # | Condition                                          | `:authority`                                         |
| - | -------------------------------------------------- | ---------------------------------------------------- |
| 0 | callee `Endpoint` carries a `volo_grpc::client::Authority` tag | the tag's value verbatim                  |
| 1 | `tls_config(ClientTlsConfig::new(server_name, ..))` | `server_name`, plus `:<port>` of the dialed address unless the port is 443 |
| 2 | callee `service_name` is a valid authority          | `service_name` verbatim                              |
| 3 | otherwise                                          | the dialed `ip:port` (`localhost` for Unix sockets)  |

`:scheme` is `https` when TLS is configured and `http` otherwise.

### Examples

Talking to a TLS-terminating proxy that routes on virtual host:

```rust
let client = GreeterClientBuilder::new("grpc.internal.example.com:50051")
    .tls_config(ClientTlsConfig::new("grpc.internal.example.com", connector))
    .build();
// SNI: grpc.internal.example.com
// :authority: grpc.internal.example.com:50051   (rule 1)
// :scheme: https
```

Plain gRPC with the default DNS resolver — the service name *is* the host:

```rust
let client = GreeterClientBuilder::new("grpc.internal.example.com:50051").build();
// :authority: grpc.internal.example.com:50051   (rule 2)
```

Plain gRPC with a custom `Discover` and a logical service name:

```rust
let client = GreeterClientBuilder::new("user-service")
    .discover(my_discover)
    .build();
// :authority: user-service                      (rule 2, same as grpc-go / tonic would send)
```

Fixed address, no usable name:

```rust
let client = GreeterClientBuilder::new("").address(addr).build();
// :authority: 10.0.0.7:8080                     (rule 3)
```

Overriding, when the name the server must be addressed by differs from all of the above (a mesh
sidecar routing on a cluster name while TLS terminates at the sidecar, say). The tag lives on the
callee `Endpoint`'s `faststr_tags`, so it can be set per call through `CallOpt`, or per client by
a layer that runs before the transport:

```rust
use volo_grpc::client::{Authority, CallOpt};

let mut opt = CallOpt::new();
opt.callee_faststr_tags
    .insert::<Authority>(FastStr::from_static_str("users.mesh.local:50051"));
client.clone().with_opt(opt).get_user(req).await?;
// :authority: users.mesh.local:50051            (rule 0)
```

A tag that is not a valid authority (empty, userinfo, a scheme) is ignored and the derived
default applies.

Rule 1 wins over rule 2 because SNI and `:authority` must agree for any TLS-terminating proxy;
the default port 443 is omitted the way HTTP clients omit it for `https`. An IP literal as
`server_name` is formatted as a socket address (`[::1]:8080`).

### Behaviour change

Before this design, `:authority` was always the resolved `ip:port`, because the same string was
also hyper's pool key and dial target. Plaintext clients whose `service_name` is a logical name
(e.g. `hello`) now send that name instead of `127.0.0.1:8080`. gRPC servers ignore `:authority`
unless they route on it; if yours does, this is the value it will see.

## 3. Connection pooling

Connections are keyed by the callee `Address` — the instance picked by the load balancer for this
call — so client-side load balancing across several instances keeps one connection per instance,
whatever `:authority` says. The transport holds a `volo::pool::Pool<Address, Http2Connection>`
in `Mode::Shared`:

- one HTTP/2 connection per address, all calls to that address multiplexed on it;
- concurrent first calls to a new address wait for the single in-flight handshake rather than
  each dialing — and that handshake runs as its own task, so a caller hitting its rpc timeout
  does not cancel the connection everyone else is waiting for;
- a connection the peer closed is noticed on the next call and replaced; a request that hyper
  hands back untouched because the connection died underneath it is retried once on a fresh one;
- connections idle for 90 seconds are closed by the pool's background task.

Timeouts (`connect_timeout`, `read_timeout`, `write_timeout`) and the `http2_*` settings on
`ClientBuilder` apply per connection exactly as before.

## 4. `volo::pool` for extension developers

The pool lives in the core crate (`volo/src/pool/`) so every protocol crate can share it. Using
it takes two impls:

```rust
use volo::pool::{Mode, Pool, Poolable, Reservation};

// 1. The transport that lives in the pool.
#[derive(Clone)]
struct MyConn(/* ... */);

impl Poolable for MyConn {
    async fn reusable(&self) -> bool { /* still usable? */ true }

    // Only for multiplexed transports; the default is exclusive use.
    fn reserve(self) -> Reservation<Self> { Reservation::Shared(self.clone(), self) }
    fn can_share(&self) -> bool { true }
    fn try_checkout(&self) -> Option<Self> { Some(self.clone()) }
}

// 2. Something that makes one: any `motore::UnaryService<K, Response = MyConn>`.
#[derive(Clone)]
struct MyConnector;

impl motore::UnaryService<volo::net::Address> for MyConnector {
    type Response = MyConn;
    type Error = MyError;
    async fn call(&self, addr: volo::net::Address) -> Result<MyConn, MyError> { /* dial */ }
}

let pool = Pool::new(volo::pool::Config::default().idle_timeout(Duration::from_secs(90)));
let conn = pool.get(addr, Mode::Shared, MyConnector).await?; // Result<Pooled<_, MyConn>, pool::Error<MyError>>
```

- `Mode::Unique` transports (one request at a time, like thrift ping-pong) are returned with
  `Pooled::reuse().await` after use; dropping the `Pooled` without it discards the transport.
- `Mode::Shared` transports need nothing after use; the pool keeps its own copy.
- `pool::Error<E>` is generic over the connector's error: `Connect(E)` when making a transport
  failed, `Canceled` when this caller was waiting on someone else's connect that failed (or the
  pool was dropped). Map it to your crate's error type; volo-grpc does
  `impl From<pool::Error<Status>> for Status`.
- `Pool::new` is safe to call outside a runtime (in a `LazyLock`, say); the idle-eviction task is
  started on the first `get`.

`volo-thrift` and `volo-http` still carry their own copies of this pool design; migrating them to
`volo::pool` is a follow-up.
