# volo-http

High-performance async HTTP client and server framework built on the Volo ecosystem, using `motore` service abstractions and `hyper` for HTTP transport.

## Directory Structure

```
volo-http/src/
├── lib.rs              # Crate entry, module exports, prelude
├── body.rs             # Body type (Full, Incoming, Stream, BoxBody) and BodyConversion trait
├── request.rs          # Request type aliases and utilities
├── response.rs         # Response type alias
├── context/            # RPC contexts
│   ├── client.rs       # ClientContext (target, stats, timeout)
│   └── server.rs       # ServerContext (RpcInfo, path params, extensions)
├── error/
│   ├── client.rs       # ClientError
│   └── server.rs       # ExtractBodyError
├── utils/              # Shared utilities (consts, cookie, extension, json, macros)
├── server/
│   ├── mod.rs          # Server struct
│   ├── handler.rs      # Handler trait
│   ├── extract.rs      # FromContext, FromRequest extractors
│   ├── middleware.rs    # from_fn, map_response
│   ├── param.rs        # PathParams, PathParamsMap, PathParamsVec
│   ├── panic_handler.rs
│   ├── protocol.rs     # HTTP1/HTTP2 config
│   ├── span_provider.rs
│   ├── route/          # Router, MethodRouter, Route, Fallback
│   ├── response/       # IntoResponse, Redirect, SSE
│   ├── layer/          # BodyLimitLayer, FilterLayer, TimeoutLayer
│   └── utils/          # client_ip, file_response, serve_dir, multipart, ws
└── client/
    ├── mod.rs          # Client, ClientBuilder
    ├── request_builder.rs
    ├── callopt.rs      # Per-request call options
    ├── cookie.rs       # Cookie jar (feature: cookie)
    ├── dns.rs          # DNS resolver
    ├── loadbalance.rs
    ├── target.rs       # Request target (address/host)
    ├── layer/          # Timeout, Host, UserAgent, FailOnStatus, HttpProxy
    └── transport/      # Connector, HTTP1/2, connection pool, TLS
```

## Key Components

### Body

`Body` wraps `Full<Bytes>`, `Incoming`, `Stream`, or `BoxBody`. The `BodyConversion` trait provides `into_bytes()`, `into_vec()`, `into_string()`, `into_faststr()`, and `into_json<T>()`.

### Server

**Routing**: `Router` maps paths to `MethodRouter`s. Supports `.route()`, `.nest()`, `.fallback()`. `MethodRouter` dispatches by HTTP method (`get`, `post`, etc.).

**Handlers**: Async functions with extractors as parameters. Extractors implement `FromContext` (non-consuming, from context/parts) or `FromRequest` (consuming, includes body -- must be last parameter). Return types implement `IntoResponse`.

**Built-in extractors**:

- From context: `Uri`, `Method`, `Address`, `HeaderMap`, `PathParams<T>`, `PathParamsMap`, `PathParamsVec`, `Query<T>`, `Extension<T>`
- From body: `Json<T>`, `Form<T>`, `Bytes`, `String`, `Vec<u8>`, `Request<B>`, `Multipart`, `WebSocketUpgrade`

**Middleware**: `from_fn` wraps an async function with `(cx, req, next) -> Response` signature. `map_response` transforms responses. Apply via `.layer()` on `Router` or `MethodRouter`.

**Server layers**: `BodyLimitLayer`, `FilterLayer`, `TimeoutLayer`

### Client

`ClientBuilder` configures and builds a `Client` with connection pooling, timeouts, and DNS resolution. `RequestBuilder` (via `client.get()`, `.post()`, etc.) builds individual requests with headers, JSON body, etc.

**Client layers**: `Timeout`, `Host`, `UserAgent`, `FailOnStatus`, `HttpProxy`

## Feature Flags

```toml
default = ["default-client", "default-server"]
default-client = ["client", "http1", "json"]
default-server = ["server", "http1", "query", "form", "json", "multipart"]
```

| Feature           | Description                                   |
| ----------------- | --------------------------------------------- |
| `client`          | HTTP client support                           |
| `server`          | HTTP server support                           |
| `http1`           | HTTP/1.1 protocol                             |
| `http2`           | HTTP/2 protocol                               |
| `query`           | Query string extraction (requires serde)      |
| `form`            | Form body extraction (requires serde)         |
| `json`            | JSON body extraction/response (uses sonic-rs) |
| `json-utf8-lossy` | Lossy UTF-8 handling for JSON                 |
| `cookie`          | Cookie support for client and server          |
| `multipart`       | Multipart form data support                   |
| `ws`              | WebSocket support                             |
| `tls` / `rustls`  | TLS via rustls                                |
| `native-tls`      | TLS via native-tls                            |
| `full`            | All features enabled                          |
