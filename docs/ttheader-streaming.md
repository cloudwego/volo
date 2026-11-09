# TTHeader streaming 接入与迁移

本实现提供 Kitex TTStream 的 Header、DATA、Trailer、RST 和 META 协议，以及同一服务中普通 Thrift unary 与 streaming 方法。服务端只在首帧识别 TTHeader streaming 标志，其他字节交给原 unary codec；不做 TTHeader/gRPC 跨协议嗅探，也不提供 Thrift over gRPC。

## 版本与生成变化

候选生成器配套为 `pilota-build 0.14.0-alpha.1`、`volo-build 0.13.0-alpha.1`。这两个 0.x build crate 的 minor 变化包含 breaking change；当前候选源码不代表已发布正式 registry 包。仓库根 `Cargo.toml` 的 `[patch.crates-io]` 将配套 Pilota 包指向 `feat/ttheader-streaming-20261105` 分支，`Cargo.lock` 固定解析出的具体提交；在仓库根目录直接执行 `cargo check` 即可，不需要外部 Cargo 配置或本机源码路径。下游独立 workspace 需要在自己的根 manifest 配置相同 patch，Cargo 不继承依赖仓库的根 patch。ShmIPC feature 随主干使用 `shmipc 0.3`，生成器测试继承 workspace 的同一版本；本轮相关验收仅要求编译通过。

升级生成器后：

- 没有 streaming 注解的方法保持普通 unary 签名；`streaming.mode="unary"` 也走普通 TTHeader framed。
- `client`、`server`、`bidirectional` 注解生成流式 API，DATA 是原始 Thrift struct，不带 unary Args/Result/消息 envelope。
- 流式方法要求一个 struct 请求参数和 struct 响应；继承方法与 Arc/typedef 包装保留。
- 任一 service 中的 streaming throws/oneway 或不支持形状都会导致整个文件构建失败，包括应用没有使用的 service。unary throws 不受影响。没有增加选择生成 service 的开关。
- `ttstream` feature 只控制运行时能力，不控制上述生成签名或诊断。关闭 feature 后仍生成新 API；调用 TTStream 或注册流式方法会在 I/O/监听前返回配置错误。只有不带流式注解的原 unary API 承诺保持兼容。

依赖升级应与生成变化分开审查：只升级旧语义依赖的前置 PR 不包含这两个候选生成变化。可执行完整样例见 [ttstream-codegen](../tests/ttstream-codegen)；其中继承 fixture 同时验证 mixed Parent/Child、Arc、unary throws 和独特方法响应。

## 统一 client 和四种角色

单一生成 client 共用发现/治理配置和目标地址；普通方法使用 unary 连接，流式方法使用 TTStream 连接。无需独立 streaming endpoint。

| 位置 | 类型 | 作用 |
|---|---|---|
| 客户端发送 | `RequestSender<T>` | 发送请求并半关闭请求侧 |
| 客户端接收 | `ResponseStream<T>` | 接收响应、读取 Header/Trailer 和终态 |
| 服务端接收 | `RequestStream<T>` | 接收请求 |
| 服务端发送 | `ResponseSender<T>` | 发送响应、设置 Header/Trailer |

`BidiStreaming<Req, Resp>::into_split()` 返回客户端 `RequestSender<Req>` 与 `ResponseStream<Resp>`，不与服务端角色混用。角色内部共享同一个流状态，没有为每条消息套一层泛型 middleware，也不为每个 sid 启动转发任务。`ClientStreaming` 可 `send(&request)` 后 `close_and_recv()`；`ServerStreaming` 接收响应；bidi 可分开收发。

发送约定是在返回异步写 future 前同步编码 `&T`，后续 await 只持有编码字节；不会因借用跨 await 额外要求 `T: Sync`。同时存在发送暂存、连接和进程预算，编码前申请配额；取消/丢弃 future 会归还。已提交字节的流不会借用 unary retry 换地址重放。

示例配置（生成的 builder 名称由 IDL 决定）：

```rust
use volo_thrift::streaming::{PoolMode, StreamConfig, StreamingProtocol};

let client = InteropClientBuilder::new("Interop")
    .address(address)
    .streaming_protocol(StreamingProtocol::TtHeader)
    .streaming_config(StreamConfig {
        pool_mode: PoolMode::Long,
        ..Default::default()
    })
    .build();

let (mut tx, mut rx) = client.bidi().await?.into_split();
tx.send(&request).await?;
tx.close_send().await?;
while let Some(response) = rx.recv().await? {
    consume(response);
}
```

生成器在 service 含 streaming 方法（包括继承方法）时自动启用 TTHeader streaming。纯 unary 或旧版生成的 client 默认不构建流式连接池、建流服务和 resolver；手写 raw/generic client 需要显式调用 `.streaming_protocol(StreamingProtocol::TtHeader)`，或安装 `.streaming_service(...)`。`streaming_config`、hooks 和 layers 只保存配置，不隐式启用；`streaming_enabled()` 反映最终 builder 能力，因此协议配置在这些方法之前或之后均可。

完整 client/server 代码和验证命令在 [fixture](../tests/ttstream-codegen/src/main.rs) 与 [互操作说明](../tests/interop/kitex-streaming/README.md)。示例片段省略应用的 address/request/consume 定义，不是独立可执行文件。

## 上下文、middleware 与观察

建流阶段通过 `Service<StreamContext, OpenStream>` 扩展。`streaming_layer` 按注册顺序组合，`streaming_layer_front` 把新的 layer 放在现有链最外层；`streaming_service` 可以替换实际建流服务。需要统计发现、池等待、失败或 future Drop 的总耗时，使用最外层 Service 边界。流内的 `StreamHook` 负责 META/Header/收发/I/O 错误和一次终态；`streaming_hook` 是追加，`streaming_hooks` 是设置整条链。服务端适配器可在最终运行前按 `has_streaming_methods()` 安装框架扩展；`streaming_hook_front` / `streaming_scope_front` 保留框架扩展先于用户扩展的顺序，二者本身不启用流式协议。

`StreamContext` 是权威上下文，框架在 handler future 外设置 task-local scope；hook 显式拿 cx。handler 自己 spawn 的任务必须显式携带 `cx.scope(future)`，Tokio 不自动继承 task-local。标准 metainfo 已由 Volo 导入/导出并执行 single-hop，不需要下层适配器再做一次 hop 转换。

服务端 `close_send()` / `finish()` 可以先结束响应，再执行异步收尾。连接会保留尚未退出的 handler 的取消归属：客户端 RST、连接断开、本地 deadline 和停服期限仍会取消 handler，但不会改写已提交的响应终态、重复触发 `on_finish` 或在 Trailer 后补发 RST。`max_streams` 同时限制连接中的未退出 handler 数；graceful shutdown 等待这些 handler 返回，达到排空期限后取消。已结束的 sid 被复用时，旧 handler 会被取消，其清理不会影响新流。

`cx.scope(...)` 只建立上下文作用域，不自动终止 spawned task。需要响应请求取消的任务应显式 `select!` 监听 `cx.cancelled()`，或通过持有并回收任务的容器管理；作用域内发起的下游 TTStream 调用会自动关联父流并级联取消。

`cx.status()` 反映协议终态；`on_finish` 还需等待排队 DATA 被应用交付/丢弃以及必要控制帧 flush/discard，因此不能把 wire Trailer 到达等同于全部 receive 已观察。单响应和泛化转换通过 `ReceiveValidation` 延后最终观察。无 hook/空 hook 链不会分配事件、读取 hook 专用时钟或进行 hook 虚调用；公共 context 计时和预算原子计数仍存在。

## 线上语义与 Rust 增强

- 建流写完请求 Header 即返回，不等待响应确认。服务端 Header 延迟到显式 send_header、首次发送或关闭响应时发送；半关闭请求仍可接收响应。
- TTStream 不在线上传播 deadline。客户端使用本地 deadline，到期发送 RST；服务端只依据自身配置、当前上下文和 RST 级联取消。不添加私有 timeout header。单次 recv 的 KeepOpen 超时可以继续接收，Cancel 超时会取消远端；两者不覆盖独立的全流 deadline。
- `StreamError` 区分 Biz、Thrift ApplicationException、带 ttscp 路径的取消、传输阶段/是否已提交，以及本地配置/资源错误。Biz 对应 Trailer 的三个 biz 字段，IDL throws 不作为类型化流异常传输。
- Kitex 使用无界接收队列，Rust 使用可配置预算；超限 RST=6 是增强，合法突发在默认预算内正常通过。默认每流接收64MiB/4096消息、每连接256MiB、共享接收1GiB；发送暂存分别64MiB/256MiB/1GiB。它们不是 RSS 上限，应用保留的解码对象/Bytes 克隆和 SDK 缓冲需要另计。
- 同连接不能根据当前只有一个活跃流推断独占。默认不暂停 socket 读；显式开启时也只允许所有活跃队列都满时有界暂停。控制帧有独立预留，超限不能使其他流永久挂起。
- signed sid 接受负数和0；未知 sid 非 Header 忽略。重复活跃 sid 回 RST=7并隔离，是明确增强。最终服务/方法查找失败回 UnknownMethod Trailer，避免复刻 Go 的等待超时；查找逻辑仍保留唯一方法、fallback/combine/unknownSvc规则。
- Long 池默认空闲60秒，Rust容量/等待配置是本地增强，Go DefaultLongConnConfig没有对应容量字段。`pool_wait_timeout`仍是整个acquire（包括本调用dial/Header）的外层时限；`pool_wait_nanos`仅统计同地址opening锁及容量队列等待，不包含自己的dial/Header工作。取消可能先返回本地错误，再等待旧连接RST flush后归还租约；新请求可临时扩池，空闲连接之后按TTL淘汰。
- 优雅停服立即拒绝新 Header，已有流继续排空；期限到后回RST=6并有限flush。Go没有可依赖的“准备关闭”通知，连接断开按本地12013处理。Go/netpoll客户端某些EOF包装为12004，互操作报告单独记录此差异。

标准 metainfo 固定 single-hop。如果原实现会把上游 transient 继续序列化，迁移中间节点后，下游不会再收到超出一跳的值。应用应显式设置当前跳 transient 或使用 persistent。

## 迁移顺序与传输

1. 升级生成器并修复带流式注解的方法签名、throws和参数形状；普通 unary 方法继续保留。
2. 在仍为 Go 的服务端上先让所有 streaming 调用方切到 TTHeaderStreaming。普通 unary 不需改协议；仍走 gRPC streaming 的调用方必须先迁移，因为 Rust不会嗅探gRPC。
3. 再替换为 Rust 服务端。同一地址的 TTHeader unary/streaming 自动分派，支持 mixed service；纯 unary 服务不做首帧预读。
4. 按实例内存和业务突发量核对预算、池容量、取消率、流时长与资源指标，使用实际候选版本运行 [Go双向互操作](../tests/interop/kitex-streaming/README.md)。

TCP/UDS/ShmIPC 使用同一分派器。ShmIPC 有独立协议池命名空间，避免复用仍绑定 unary 的 SDK logical stream；相关运行验证和修复本轮延期，仅完成编译验证。服务网格集成和实际部署验收未执行。

## 本地验证范围

已完成本地 Linux 的协议、错误、取消、路由、继承与分派验证，以及配套 fmt、Clippy 和 Rust 1.85 检查。TCP/UDS 的最终 600 秒资源测试通过；历史失败报告保留。ShmIPC 运行验证、远端跨平台 CI 与实际部署验收未执行。代码分支推送不代表 crate 已发布。

2026-11-09 同步主干后，全 workspace/all-targets 的 Linux nightly 构建通过，覆盖 TTStream、multiplex 与 ShmIPC feature；关闭 TTStream 的库检查和 131 项 TTStream 相关测试也通过。运行时及生成器库的严格 Clippy 通过；生成器全 target 检查仍受主干已有的 `config_builder.rs` 测试模块位置诊断阻断。此前 Rust 1.85 和性能报告仍对应原候选，本次未重新执行这些矩阵。
