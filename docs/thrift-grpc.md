# Thrift over gRPC

这是当前候选源码的使用说明，尚未发布。生成器与 runtime 必须使用同一组 Volo/Pilota 候选源码；远端分支尚未交付时，本地验证使用显式 Cargo patch。依赖来源、未完成项见 [实施状态](thrift-streaming-over-grpc-status.md)。Cargo Git 依赖使用分支名，解析 SHA 仅记录在 lock/验证 manifest。

本入口支持普通 unary、显式 unary、client/server/bidirectional streaming。它只使用 HTTP/2 gRPC，不对 TTHeader、TTStream 或其他协议做自动嗅探。

## 生成和协议

启用 `volo-grpc/thrift`，build.rs 使用独立入口：

```rust
fn main() {
    volo_build::Builder::thrift_grpc()
        .add_service("idl/echo.thrift")
        .write()
        .unwrap();
}
```

也可以执行 `volo init echo --rpc-transport grpc idl/echo.thrift`。配置入口仍使用 `protocol: thrift`，在该 entry 的 service 配置中指定传输；一个 entry 不能混用两种 Thrift 后端：

```yaml
entries:
  default:
    filename: volo_gen.rs
    protocol: thrift
    services:
      - idl:
          source: local
          path: ../idl/echo.thrift
        codegen_option:
          config:
            rpc_transport: grpc
```

workspace 使用 `volo_build::workspace::Builder::thrift_grpc()`。它自动声明 `volo-grpc` 的 Thrift feature，保留已有依赖的显式版本和 feature。原 `.thrift()` / `.protobuf()` 的含义不变。

| IDL 模式 | 请求 payload | 响应 payload | Rust handler 输入/输出 |
| --- | --- | --- | --- |
| 无注解 / `unary` | 方法 Args | 方法 Result | `Request<FooMethodRequest>` / `Response<Ret>` |
| client | 原始 Req struct | 原始 Resp struct | `Request<ThriftRecvStream<Req>>` / `Response<Resp>` |
| server | 原始 Req struct | 原始 Resp struct | `Request<Req>` / `Response<SendStream<Resp>>` |
| bidirectional | 原始 Req struct | 原始 Resp struct | `Request<ThriftRecvStream<Req>>` / `Response<SendStream<Resp>>` |

handler 的外层返回值均为 `Result<Response<_>, Status>`。普通 unary 可以零/多个参数；显式 `streaming.mode="unary"` 必须恰好一个参数，与本轮固定 Kitex/thriftgo 相同。多参数方法保留无注解。

所有 service 都生成并校验。任意 service 的 streaming 方法含 `throws`，整个入口失败；新入口也拒绝 oneway。一个 service 的 payload kind 同质，不提供方法级 Thrift/Protobuf 混编；两类独立 `NamedService` 可以挂到同一个 server。

每条 gRPC message 是 5 字节前缀和 Thrift Binary payload，没有 Thrift message envelope，也没有 TTHeader。空 struct 是一个 STOP 字节 `00`。unary 的单 struct 参数仍位于 Args 字段内，不能直接发送 struct。wire path 按固定 thriftgo 的 Go package/service/原始方法名规则生成，Rust namespace 或重命名不改变该路径；特殊生成器可显式配置 `wire_package` / `wire_services`。

服务端为新 Thrift 路由要求 `application/grpc+thrift`。其他 gRPC subtype 返回 Unimplemented；非 gRPC content-type 返回 HTTP 415。校验发生在路由后、消息解码前。既有 Protobuf 路由保持原行为。

## unary 声明异常

有 `throws` 时，输出为 `Response<Result<Ret, FooMethodException>>`。单个和多个异常都使用方法级枚举；每个 variant 携带对应 Pilota 异常结构体，保留原 field ID：

```rust
// handler：返回其中一个声明异常，外层 RPC 仍成功。
Ok(Response::new(Err(EchoCallException::Failure(failure))))

// client：先处理 RPC 错误，再匹配成功值/声明异常。
match client.call(request).await?.into_inner() {
    Ok(value) => consume(value),
    Err(EchoCallException::Failure(error)) => handle_failure(error),
    Err(EchoCallException::Other(error)) => handle_other(error),
}
```

void 的成功值为 `()`。0→1 个 throws 会增加内层 Result；1→多个 throws 保持枚举类型形状，但新增 variant 会影响穷尽 match。streaming 不传递类型化 throws。

## 发送、接收与取消

`SendStream::from_stream` 接受 owned `Stream<Item = Result<T, Status>>`。`thrift::channel` 返回一个非 Clone 的 `Sender<T>` 和 `SendStream<T>`；Drop sender 表示请求半关闭。半关闭不取消响应方向。

`sender.send(&value)` 是同步准备、异步提交：函数返回前完成 Thrift 编码，返回的 future 只借用发送器并持有编码结果，不持有 `&T`，因此不要求 `T: Sync`。future 存活时不能再次借用同一个 sender；需要多生产者时，在应用边界做显式汇聚。

同时驱动发送与接收，避免有界队列/H2 流控与对端处理顺序组成循环等待：

```rust
let (mut tx, input) = volo_grpc::thrift::channel(16);
let sending = async move {
    for message in messages {
        tx.send(&message).await?;
    }
    drop(tx);
    Ok::<_, volo_grpc::Status>(())
};
let receiving = async {
    // 建流可能等待对端初始 Header；必须允许发送 future 同时前进。
    let mut rx = client.bidi(input).await?.into_inner();
    while let Some(message) = rx.message().await? {
        consume(message);
    }
    Ok::<_, volo_grpc::Status>(())
};
futures::try_join!(sending, receiving)?;
```

先写完全部请求，再开始读响应，可能在双方窗口/队列都满时互相等待。仅在 `client.bidi(input).await` 返回后才首次使用 tx，也可能因 Go 延迟发送 Header 而等待。上面的并行驱动涵盖两种情况；`try_join!` 不需要额外 spawn。实际网络对照见 `tests/thrift-grpc-codegen/tests/application_flow.rs`：4096 条消息、1 KiB H2 窗口下，同任务先发后收停滞；取消后，同一 client 的并发收发完成全部消息。执行入口为 `cargo test -p thrift-grpc-codegen --test application_flow`。

unary 请求严格等待一个消息及 END_STREAM；unary/client-streaming 响应严格验证一个消息和最终成功 Trailer。server-streaming 首条请求到达即进入 handler，不等待请求 EOS，额外请求按 Kitex 行为忽略。已打开请求和额外 DATA 的实际对照见 `server-open-request` / `server-two-requests` raw fixture。

最终 RPC 状态以 Trailer 为准。响应 Header 返回不表示成功；Drop 未完成的响应、`StreamContext::cancel()`、deadline、RST 或断连会结束该调用。每个请求/响应 handle 与 extensions 中的 `StreamContext` 指向同一个调用状态。

handler、producer 及框架管理的任务在相同 context/metainfo scope 内执行。需要随调用回收的任务使用 `cx.spawn_scoped(...)`；裸 `tokio::spawn` 不自动继承 context、task-local 或取消关系。`cx.children_reclaimed().await` 可等待受管理任务实际释放；token 置位本身不是回收证明。

## hooks、metadata 与业务状态

建流仍经过 motore Service/layer。消息与终态通过 builder 的 `stream_hook(factory)` 注册；factory 按调用创建单层 hook，sender/receiver 类型不随 hook 数量嵌套。`on_header` 与 `on_trailer` 分开，`on_finish` 恰好一次；Header 返回后仍持续触发消息/终态事件。未注册 hook 时没有 hook 创建、虚调用或事件任务。

标准 metainfo 由 Volo 导入/导出：本地 transit/transient 发往下一跳；收到的 upstream transient 不再自动序列化；persistent 可以跨多跳。Rust 固定这一语义，没有 Kitex Legacy/bugfix flag。Go 默认 Legacy 可能继续转发 upstream transient，这是迁移差异。

使用 `cx.response_headers(...)` 和 `cx.response_trailers(...)` 发送普通响应 metadata。用户不能覆盖 `grpc-status`、`grpc-message`、`grpc-status-details-bin`、content-type 或压缩保留字段。producer 返回 `Err(Status::ok(...))` 是非法状态，会转为 Internal；不能以它结束成功流。

`thrift::BizStatus { code, message, extra }` 的 `into_status()` / `from_status()` 对应 gRPC 的 `biz-status`、`biz-extra` 和 percent-encoded `grpc-message`；不发送 TTStream 的 `biz-message`。非零业务码通过最终错误 Trailer 发送，不构造默认成功消息。声明异常仍在 unary Result 内，与 biz/status 不同。

client 默认接收 16 KiB 的 HTTP/2 header list，响应 Trailer 也受此限制。需要接收更大的业务详情时，可通过 `http2_max_header_list_size` 设置匹配的接收上限。启用 gzip 或 zlib 时，`flate2` 使用纯 Rust 的 zlib-rs 后端。编码器按片段写入同一压缩流，按实际压缩输出增长缓冲。

固定 Kitex 的未知 service 返回原始码 20，Rust 归一化为 Unknown(2)；未知 method 的原始码 1 映射 Cancelled。`Status::raw_grpc_code()` 保留对端原码。本地取消没有该对端码，不能仅按数字 1 推导用户取消或重试。

## 资源与迁移

| 配置 | 默认值 / 契约 |
| --- | --- |
| Thrift 单消息发送、接收、解压 | 各 16 MiB，`codec_limits` 可配置；超过预算为 Rust 的保护增强 |
| channel | 默认配置 32 条、64 MiB 已编码字节、单消息 16 MiB；显式 channel 参数调整条数 |
| 受管理 H2 池 | 每 endpoint 最多 4 连接、128 个已获准的排队请求、4096 endpoints、空闲 90 秒；`stream_pool_config` 可配置 |
| keepalive | 默认关闭；开启时与 Go 服务端 MinTime/PermitWithoutStream 一起配置 |
| server drain | 默认 30 秒，可用 `graceful_shutdown_timeout` 配置；到期取消并回收调用/连接 |

与本轮 Kitex 默认策略配合的长空闲 bidi，可显式选大于其 5 分钟 MinTime 的周期（例如 6 分钟），并保持 `http2_keepalive_while_idle(false)`。无活跃流也发送 PING，需要对端同时允许；不能只调整间隔。加速的真实 Go 合法/过密 PING 对照与未 ACK 测试分别保存，不将加速用例描述成生产默认周期验收。

队列预算约束传输层已获准的请求；等待许可的 future 仍由调用方拥有，受其 deadline/取消约束。已提交业务字节的调用不自动重放；连接 GOAWAY/断开只影响连接复用和后续建流选择。identity 与 gzip 分别计量：gzip 会生成压缩输出，当前实现还会线性化输入，不把它归为零拷贝。

Go 调用方须显式选择 `transport.GRPC`，覆盖同 service 的普通 unary 和 streaming。只选 GRPCStreaming 不能保证普通 unary 使用 gRPC。替换前核对 wire package/method、Args/Result、throws enum 和 metadata 单跳差异；排空旧调用后再切换，回退时同样不重放已提交调用。TTHeader/TTStream 调用方须先改协议，当前 listener 不承担共端口嗅探迁移。

本轮未执行真实发布、生产迁移/回滚、服务网格或 ShmIPC 运行验证。远端 CI 自 2026-11-11 起恢复正常触发，历史未执行记录不计 PASS。性能、候选依赖交付及其余门禁以实施状态中的实际结果为准。

H2 传输还提供 `StreamHook::on_flow_control(cx, wait)`：仅记录实际窗口耗尽后恢复（包括 peer reset）的等待；不包含 producer 或 channel 排队，取消时未恢复的等待由终态负责。无注册 hook 的路径不读时钟。

普通自定义 metadata 可以有多个值，`-bin` 值按不透明字节处理。与 Kitex 读取重复响应字段时使用支持 `metadata.MD` 的 legacy API；StreamX 的单值 Header/Trailer 接口有自己的多值转换限制。畸形Thrift在固定Kitex上可能以wire 7（Thrift ProtocolError type ID）返回，排查时结合对端错误文本，不能只凭数值判断权限问题。
