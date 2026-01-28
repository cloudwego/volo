# Volo-Thrift Multi Service 使用指南

## 1. 功能概述

Multi Service 功能允许一个 volo-thrift Server 同时处理多个 Thrift Service，通过 TTHeader 中的 `isn` (IDL Service Name) 字段进行路由。

### 主要特性

- **多服务支持**：单个 Server 可注册多个 Thrift Service
- **ISN 路由**：根据请求头中的 `isn` 字段自动路由到对应服务
- **默认服务**：支持设置默认服务处理无 ISN 或未知 ISN 的请求
- **零拷贝**：基于 `Bytes` 的请求/响应传递，避免重复序列化
- **向后兼容**：单服务模式 API 保持不变

## 2. 快速开始

### 2.1 定义多个 Thrift Service

```thrift
// hello.thrift
namespace rs hello

struct HelloRequest {
    1: required string name,
}

struct HelloResponse {
    1: required string message,
}

service HelloService {
    HelloResponse hello(1: HelloRequest req),
}
```

```thrift
// echo.thrift
namespace rs echo

struct EchoRequest {
    1: required string message,
}

struct EchoResponse {
    1: required string message,
}

service EchoService {
    EchoResponse echo(1: EchoRequest req),
}
```

### 2.2 实现 Service Handler

```rust
use volo_gen::hello::{HelloService, HelloRequest, HelloResponse};
use volo_gen::echo::{EchoService, EchoRequest, EchoResponse};

// HelloService 实现
#[derive(Clone)]
struct HelloServiceImpl;

impl HelloService for HelloServiceImpl {
    async fn hello(
        &self,
        req: HelloRequest,
    ) -> Result<HelloResponse, volo_thrift::ServerError> {
        Ok(HelloResponse {
            message: format!("Hello, {}!", req.name).into(),
        })
    }
}

// EchoService 实现
#[derive(Clone)]
struct EchoServiceImpl;

impl EchoService for EchoServiceImpl {
    async fn echo(
        &self,
        req: EchoRequest,
    ) -> Result<EchoResponse, volo_thrift::ServerError> {
        Ok(EchoResponse {
            message: req.message,
        })
    }
}
```

### 2.3 创建 Multi Service Server

```rust
use volo_thrift::server::{Router, Server};
use std::net::SocketAddr;

#[volo::main]
async fn main() {
    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();

    // 创建各个服务（使用 from_handler 方法）
    let hello_service = volo_gen::hello::HelloServiceServer::from_handler(HelloServiceImpl);
    let echo_service = volo_gen::echo::EchoServiceServer::from_handler(EchoServiceImpl);

    // 创建 Router
    let router = Router::new()
        .with_default_service(hello_service)  // 设置默认服务
        .add_service(echo_service);           // 添加额外服务

    // 启动 Server
    Server::with_router(router)
        .run(volo::net::Address::from(addr))
        .await
        .unwrap();
}
```

## 3. API 详解

### 3.1 Router

`Router` 是多服务路由器，负责根据 ISN 将请求分发到对应的服务。

```rust
use volo_thrift::server::Router;

// 创建空路由器
let router = Router::new();

// 设置默认服务（处理无 ISN 或未知 ISN 的请求）
let router = router.with_default_service(service_a);

// 添加额外服务
let router = router.add_service(service_b);

// 查询已注册服务数量
let count = router.service_count();

// 检查是否有默认服务
let has_default = router.has_default_service();
```

### 3.2 NamedService Trait

每个服务需要实现 `NamedService` trait 以提供服务名称。代码生成器会自动为生成的 `*Server` 类型实现此 trait。

```rust
pub trait NamedService {
    /// IDL 中定义的 service 名称
    const NAME: &'static str;
}

// 自动生成的实现示例
impl<S> NamedService for HelloServiceServer<S> {
    const NAME: &'static str = "HelloService";
}
```

### 3.3 Server::with_router

使用 `Server::with_router` 创建支持多服务的 Server：

```rust
use volo_thrift::server::Server;

let server = Server::with_router(router)
    .layer(SomeMiddleware)  // 支持 layer
    .run(addr)
    .await?;
```

## 4. 路由规则

Router 按以下优先级进行路由：

1. **精确匹配**：如果请求携带 `isn` 且匹配某个已注册服务名称，路由到该服务
2. **默认回退**：如果请求无 `isn` 或 `isn` 不匹配任何服务，路由到默认服务
3. **错误处理**：如果无默认服务且无法匹配，返回 `ApplicationException(UNKNOWN_METHOD)`

```
请求到达
    │
    ▼
  有 ISN?
   / \
  是  否
  │    │
  ▼    ▼
匹配服务? ──否──► 有默认服务?
  │              / \
  是            是  否
  │             │   │
  ▼             ▼   ▼
路由到匹配服务  路由到默认服务  返回错误
```

## 5. 生成代码变更

代码生成器为每个 `*Server` 类型自动生成以下实现：

### 5.1 NamedService 实现

```rust
impl<S> NamedService for HelloServiceServer<S> {
    const NAME: &'static str = "HelloService";  // IDL 中的 service 名称
}
```

### 5.2 Service<ServerContext, Bytes> 实现

```rust
impl<S> Service<ServerContext, Bytes> for HelloServiceServer<S>
where
    S: HelloService + Send + Sync + 'static,
{
    type Response = Bytes;
    type Error = ServerError;

    async fn call(&self, cx: &mut ServerContext, payload: Bytes)
        -> Result<Bytes, ServerError>
    {
        // 从 context 重建消息标识（零拷贝）
        // 解码请求
        // 调用业务逻辑
        // 编码响应
    }
}
```

### 5.3 服务创建方法

```rust
#[derive(Clone)]
pub struct HelloServiceServer<S> {
    inner: S,  // 私有字段
}

impl<S> HelloServiceServer<S> {
    /// 从 handler 创建服务实例，用于多服务路由
    pub fn from_handler(handler: S) -> Self {
        Self { inner: handler }
    }
}

impl<S: HelloService + Send + Sync + 'static> HelloServiceServer<S> {
    /// 创建单服务 Server（传统用法）
    pub fn new(inner: S) -> Server<Self, ...> {
        Server::new(Self { inner })
    }
}
```

## 6. 与 volo-grpc 对比

| 特性 | volo-thrift | volo-grpc |
|------|-------------|-----------|
| 路由标识 | TTHeader `isn` 字段 | gRPC path |
| NamedService | 基于 IDL service 名称 | 基于 package.service 名称 |
| Router API | 相同 | 相同 |
| 默认服务 | 支持 | 支持 |

## 7. 注意事项

1. **ISN 编码**：客户端需要在 TTHeader 的 string KV 中设置 `isn` 字段
2. **服务名称**：使用 IDL 中定义的原始 service 名称（非 Rust 名称）
3. **Handler Clone**：Handler 实现需要实现 `Clone` trait
4. **单服务兼容**：现有单服务代码无需修改，`Server::new` API 保持不变

## 8. 完整示例

参见 `examples/tests/thrift_multi_service.rs` 中的集成测试。

### 8.1 Server 示例

```rust
use volo_thrift::server::{Router, Server};

#[derive(Clone)]
struct HelloImpl;
impl HelloService for HelloImpl { /* ... */ }

#[derive(Clone)]
struct EchoImpl;
impl EchoService for EchoImpl { /* ... */ }

#[volo::main]
async fn main() {
    let addr = "127.0.0.1:8080".parse().unwrap();

    let router = Router::new()
        .with_default_service(HelloServiceServer { inner: HelloImpl })
        .add_service(EchoServiceServer { inner: EchoImpl });

    Server::with_router(router)
        .run(volo::net::Address::from(addr))
        .await
        .unwrap();
}
```

### 8.2 测试路由逻辑

```rust
use volo_thrift::server::Router;
use volo_thrift::context::ThriftContext;
use motore::service::Service;

#[tokio::test]
async fn test_routing() {
    let router = Router::new()
        .with_default_service(hello_service)
        .add_service(echo_service);

    // 测试 ISN 路由
    let mut cx = ServerContext::default();
    cx.set_idl_service_name("EchoService".into());
    let resp = router.call(&mut cx, payload).await?;

    // 测试默认路由
    let mut cx = ServerContext::default();
    let resp = router.call(&mut cx, payload).await?;
}
```

## 9. 相关类型导出

以下类型从 `volo_thrift` 导出：

```rust
// 路由相关
pub use volo_thrift::server::{Router, NamedService};
pub use volo_thrift::server::Server;  // with_router 方法

// ISN 相关（通过 ThriftContext trait）
pub use volo_thrift::context::ThriftContext;  // idl_service_name() / set_idl_service_name()
pub use volo_thrift::codec::default::ttheader::HEADER_IDL_SERVICE_NAME;  // ISN header key ("isn")

// Bytes 类型
pub use volo_thrift::{Bytes, BytesMut};
```
