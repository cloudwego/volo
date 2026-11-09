# Volo：Thrift unary / streaming over gRPC 开发计划

状态：**本仓库有效 checklist 已完成。** 性能采用保留全部匹配样本的显式复核，历史 FAIL 不改写；实际证据与延期范围见 [实施状态](thrift-streaming-over-grpc-status.md)。源码调查：2026-11-09；开始实施：2026-11-10。

本文件是 Volo 的独立子计划，负责 G0～G6 与公共前置 PR C1～C4。协议、公开 API、生命周期及公共互操作契约在本文件维护；Pilota 的 P0/P1 在其仓库同名文档维护。下游适配只消费公共扩展点，不成为 Volo 的运行时依赖。原有 TTStream 实施记录保持独立，新 checklist 不继承旧 PASS。

## 1. 范围、模块与交付物

支持 Thrift unary、client-streaming、server-streaming、bidi over HTTP/2，与上游 Kitex v0.16.4 双向互通。unary 区分无注解和显式 unary，支持同一 Thrift service 的 unary/streaming 混合；同一 service 的 payload kind 同质，不同 Protobuf/Thrift NamedService 可共存。

| 所属模块 | 本仓库负责 | 阶段 |
| --- | --- | --- |
| `volo-grpc/src/codec`、`message.rs` | 默认兼容的 codec 泛化、Thrift Binary、framing/压缩/预算 | G0/G1 |
| `volo-grpc/src/server/{router,service,meta}.rs`、`transport/client.rs` | service 描述、内容类型、路由/解码边界及正确错误响应 | G1 |
| `volo-build`、配置/workspace/CLI | 显式 thrift_grpc 后端、四模式 API、统一异常 enum、共享消息类型 | G2 |
| `volo-grpc/src/streaming`、body、timeout、transport/server | context、hook、取消/deadline、连接池、有限 drain | G0/G3、C2～C4 |
| `volo-grpc/src/status.rs`、metadata/meta | 公共解析健壮性、标准 metainfo、biz/details | C1/G4 |
| `tests/interop/kitex-streaming` | 共用 Python runner/Go module、raw peer、Go 互通及候选报告 | G0/G5/G6 |

不实现 TTHeader/gRPC 或新 HTTP/1/HTTP/2 嗅探。仅测试既有显式 accept_http1 配置的错误路径，不改变默认 HTTP/2-only。oneway 在新后端拒绝；Compact、gRPC-Web、JSON/Map 泛化不纳入。既有 Protobuf/TTStream 的正常行为及源码兼容必须验证，公共缺陷修复的行为变化单列。

本轮在现有开发分支提交、推送源码，按 Pilota→Volo→下游整族更新分支依赖与锁。不运行真实服务网格集成、生产部署/演练，不发布 crate/tag、不合并主干。2026-11-11 起恢复远端 CI，按仓库现有规则触发，不使用跳过标记。ShmIPC 只验证涉及时的 feature 组合编译，不运行或修复其场景。延期事项标为 DEFERRED_BY_USER/NOT_EXECUTED，不计 PASS，也不阻塞本轮有效 checklist。

交付包括公共源码、四模式及混合 service 示例、完整编译/API 回归、Go 双向/故障报告、性能与资源证据、迁移和候选版本材料。只通过 codec 自回环不足以完成本计划。

## 2. 基线、源码入口与跨仓依赖

| 项目 | 已核对基线 |
| --- | --- |
| Volo HEAD | `7dd69f9839be31cf6c035e33c92316cf25991045` |
| 当前包 | volo 0.12.5、volo-build 0.13.0-alpha.1、volo-grpc 0.12.2 |
| 当前本仓库 lock | hyper 1.8.1、hyper-util 0.1.19、h2 0.4.12、http 1.4.0 |
| 数据层 | Pilota HEAD `50e79266f2dc7e78a71f7b5c9f59eccf3813f7d6`；runtime 0.13.1、build 0.14.0-alpha.1 |
| Go oracle | Kitex v0.16.4、thriftgo v0.4.5；实际工具链/module/sum 与生成选项写入 manifest |

SHA 只用于证据。Cargo.toml 使用具名 branch，Cargo.lock 记录实际解析 SHA；候选锁变化后重核来源，不能把邻接仓库 checkout 当作实际依赖。公共提交/报告不得包含私有仓库、服务标识、本机绝对路径或部署专用 profile。

| 接口/交付 | 前置输入 | 输出/使用方 |
| --- | --- | --- |
| G0 协议与 API 冻结 | 固定 Go 产物、当前 Protobuf 基线、取消/最终提交原型 | Pilota P0/P1、Volo G1～G4、下游治理/观测适配 |
| Pilota P0 | 全 service 校验、模式、原始名称/namespace、Args/Result/异常类型查询 | G1/G2；不要求新增不必要的 parser/runtime 改动 |
| Pilota P1 | 已提交 raw/Args/Result golden、Go 来源和双向解码证据 | G1/G2/G5；Go 更新统一由本仓库 runner 提供 |
| C1～C4 | 现有 Protobuf/raw peer，均不依赖 Thrift feature | 公共基线先修复，G1/G3/G4 复用后追加 Thrift 回归 |
| 候选反馈 D1 | 下游在同一候选源码/lock 上的适配报告 | G6 接收可公开的摘要和最简公共复现；不依赖下游正式发布 |
| G6 | G5 本仓库报告与 D1 反馈 | 本地候选交付；下游最终验收消费候选，避免循环依赖 |

调查基线事实（修改前）：RecvStream 绑定 pb::Message、两个分支直调 DefaultDecoder；Encoder 每消息构造但默认是 ZST；服务端没有 CT 校验且 MetaService 固定覆盖响应 CT；accept_http1 默认 false，client/server keepalive interval 默认 None。公共错误解析存在坏 base64 expect，timeout 提前写 Duration/先 poll inner，shutdown 无限等 tx.closed，详见第 10 节 R1～R4。

Kitex 对照入口为 `pkg/remote/codec/grpc`、`pkg/remote/codec/thrift/thrift_data.go`、`pkg/remote/trans/nphttp2`、`tool/internal_pkg/tpl/service_v2.go` 和 `pkg/transmeta`。thriftgo 的 `ParseStreaming`、PkgInfo 及实际 ServiceInfo 决定 IDL 限制和 wire name，不能套用 Protobuf package 规则。

## 3. 线协议契约

### 3.1 消息、请求路径和内容类型

```text
HTTP/2 request HEADERS
  :method = POST
  :path = /[$package.]$service/$method
  content-type = application/grpc+thrift
  te = trailers

gRPC message = compressed_flag:u8 + payload_len:u32be + payload
payload      = unary: Thrift Binary 的 MethodArgs / MethodResult 结构体
               streaming: Thrift Binary 的原始 Req / Resp 结构体
               （启用压缩时先编码结构体，再压缩整个结构体字节）

request END_STREAM = 请求方向半关闭
response trailers  = grpc-status + grpc-message + 可选 metadata/details/biz 字段
```

- HTTP/2 DATA frame 与 gRPC message 不要求一一对应；必须支持 prefix/payload 跨 frame，以及一个 frame 中多个 message。
- 各模式都不加入 TTHeader、Thrift framed 长度、message begin/end 或 EXCEPTION envelope。三种 streaming 不加 Args/Result 包装；unary 必须编码 Args/Result，其中 Result 包含 success 或声明的异常字段。空结构体也需合法 Thrift STOP；0 字节 payload 不能套用 Protobuf 空消息规则。
- 编码使用 Binary 大端；字段、容器、unknown field、optional/required、浮点边界复用 Pilota 规则。确定性数据逐字节比较，map/set 允许合法顺序差异。
- `:authority`/服务发现目标与 IDL service 是不同概念；部署服务名不用作 wire service，Rust 模块路径不替代 Go PackageName。路径对方法原始大小写敏感。
- G0 提交 namespace go、无 namespace、跨文件 include/继承、重命名的 ServiceInfo/path golden。Kitex `service_v2.go` 使用 thriftgo 的 `PkgInfo.PkgName`，不能套用 Protobuf 后端的 `file.package`；必须逐个继承方法记录 wire service、PackageName 归属的 IDL 文件和实际 Go ServiceInfo。默认按固定 Kitex/thriftgo 产物生成，保留显式 wire package/service 名覆盖以对接历史生成配置；不猜测 namespace 点号拼接规则。
- Thrift 路由只接受 `application/grpc+thrift`，不从 payload 猜 codec。`application/grpc`/`+proto` 仍归现有 Protobuf；已选 Thrift 方法遇到其他 gRPC subtype 返回 `Unimplemented(12)`，非 gRPC content-type 返回 HTTP 415，不进业务 handler。均记录为配置/能力不匹配，不回退协议。
- Thrift 的请求 Header、响应初始 Header 和 trailer-only 响应均保留正确内容类型，不能被 MetaService 或 Status 错误回包路径覆盖成 Protobuf 默认值；已有初始 Header 的独立终态 Trailer 不重复插入 content-type。
- 服务按声明的 wire service/method 显式注册；重复路径在构建时失败。G0 验证 Go 的包名处理、strict service 查找与可配置 unknown-service fallback；默认使用严格已注册路径。Go 的宽松包名/fallback 行为若未实现，应作为明确差异记录，不承诺复制 TTStream 的唯一方法兜底。

content-type 的接线顺序固定为 **Router 选中 NamedService → 解析该 service 的方法描述 → 校验内容类型 → 压缩设置与 `T::from_body` → 应用 Service/handler**。校验在路由与 CodecService 解码入口之间完成，不能在全局 MetaService 提前猜 codec，也不能先解码再判断。未注册 service/method 先走明确的路由错误；未知路径、缺失/重复/畸形 content-type 及错误 subtype 的优先级在 G0 fixture 固定。

这是一项新增 Thrift 路由能力，现有 Protobuf 路由的宽松接收行为保留基线，不借此收紧所有旧服务。路由描述须传到 MetaService 的正常/早期错误路径；用内部响应策略携带 HTTP 状态与 content-type，保持现有 Service 的 `Error = Status` 接口。HTTP 415 必须真实写入 HTTP status，不能仅调用默认 HTTP 200 的 `Status::to_http()`，也不能让外层 MetaService 覆盖已选内容类型。

`accept_http1` 当前默认关闭；维持默认值。本轮不新增连接嗅探，仅对既有显式 `accept_http1(true)` 分支增加 HTTP/1.1 POST 到已知 Thrift 方法、`content-type: text/plain` 的 415 fixture，与 HTTP/2 同输入对照，均断言 decoder/handler 调用数为零。默认 HTTP/2-only 收到 HTTP/1 的行为按基线验证，不宣称默认会回 415。

### 3.2 错误和终态

对外继续使用 `volo_grpc::Status`，提供 `thrift` 模块下业务错误构造/读取 helper，不强迫所有 gRPC layer 接受新的错误 enum。

| 事件 | gRPC 表达与 Rust 行为 |
| --- | --- |
| 成功结束 | `grpc-status=0`，收完消息后验证终态；HTTP 200、消息 EOF 或收到一个 Resp 均不能单独证明成功 |
| 业务错误 | `biz-status` 为业务码、`biz-extra` 为 JSON map、消息来自 `grpc-message`；默认配非零 gRPC `Internal(13)`，显式指定其他合法非 OK code 时保留。不发送 TTStream 的 `biz-message` |
| 业务状态读取 | 保留 code/message/extra、原始 metadata 和 gRPC code；data 后出错、trailer-only 都应可见。防止 biz 信息被通用 Status 转换丢失 |
| unary IDL throws | 声明的异常作为 MethodResult 的对应字段发送，正常 `grpc-status=0`；客户端验证最终 Trailer 后返回类型化异常。与 biz-status、ApplicationException、gRPC Status 分开处理 |
| 普通 handler/Thrift 编解码异常 | 默认转 `Internal(13)` + 诊断；不把 Thrift ApplicationException 编到 DATA 当作 Resp，不自创恢复异常 type-id 的 header |
| 显式 gRPC Status | 原样保留 code/message/details/metadata。`grpc-status-details-bin` 仍是标准状态详情，与数据 payload 使用 Thrift 无冲突 |
| 远端 Kitex unknown service | 未配置 fallback 且进入 UnknownService 分支时，TransError type-id=20 经 `convertStatus` 写出 `grpc-status=20`；Volo `Code::from_bytes` 归一化为 `Unknown(2)`，诊断保留原始 20 |
| 远端 Kitex unknown method | 成功匹配 service、进入 UnknownMethod 分支时 type-id=1，写出 `grpc-status=1`，Volo 解析成 `Cancelled(1)`；这是远端错误映射，不能据此断言用户主动取消，也不按消息文本擅自改码 |
| 本地主动取消 / 本地 deadline | 分别 `Cancelled(1)` / `DeadlineExceeded(4)`，对端通常观察到 RST_STREAM 或 context 结束；不要求竞态两端错误码完全相同 |
| 连接断开 | 无成功 Trailer 时不得成功 EOF；报告 `Unavailable(14)` 或实际底层映射，保留传输原因和是否已提交请求 |
| 消息/队列/解压预算超限 | 本地 `ResourceExhausted(8)`；服务端能够回 Trailer 时回该状态，无法写回时 reset 当前 H2 stream。Go 端异步可见结果由 fixture 固定 |
| 未注册方法/服务 | Rust 回 `Unimplemented(12)`、handler 不执行；与 Kitex 特殊错误映射的差异单列，不复制不标准的 code 作为默认 Rust API |
| 错误状态发出后 | 不再发成功 Trailer；全流 finish 一次。收到终态后额外 DATA/重复 Trailer 按协议错误处理，不重新进入业务 |

`biz-status=0`、有 biz 字段却 `grpc-status=0`、非法 JSON、越界数字、重复保留字段和 details 与 code 不一致必须有 fixture。默认业务错误 helper 拒绝生成含糊组合；接收端不得 panic 或把明确非零 gRPC 状态吞成成功。Go 原始错误类型、码与 Rust 结果分别记录；unknown service/method 分别以真实 Go 与 raw peer 验证，并记录 SearchService/fallback 配置，不能合并为一个 unknown case。

`biz-extra` 使用 JSON 原始 UTF-8 字节作为 header value，不套用 `grpc-message` 的 percent-encoding，也不改成 base64。Go `Map2JSONStr` 的正常实现及 fallback 对引号、反斜杠和控制字符做 JSON 转义；仍须记录真实 HPACK 值，区分 JSON 内的转义序列与非法原始 CR/LF/NUL。Rust 用经过校验的 header bytes 构造及 JSON bytes 解析，不能经 `HeaderValue::to_str()` 的 ASCII 检查而丢失合法中文。中文 key/value、引号/反斜杠、换行/制表/NUL 的转义、坏 JSON/非法 UTF-8、大小上限均覆盖；语义比较不依赖 map 顺序。通用 metainfo 只对自身前缀做文本转换，其他合法不透明 metadata 原样保留。

非法 `grpc-status-details-bin` 必须由 `Status::from_header_map` 返回解析错误状态，冻结为 `Internal(13)`，保留有界原始状态诊断，不 `expect`、不静默清空 details 后当作成功。保持 `Option<Status>` 公共签名，修复覆盖未启用 Thrift 的公共 Protobuf 路径；分别测试初始 trailer-only 与正常最终 Trailer、原始 code=0/非零、合法带/不带 padding 和非法 base64。

### 3.3 模式、Header 时序与半关闭

- unary：客户端发送一个 MethodArgs 后 END_STREAM，成功或声明异常返回一个 MethodResult 加成功 Trailer；Status/biz 错误允许 trailer-only。客户端方法 future 收完唯一响应及终态才结束，不能收到 Header/首条 DATA 就返回。详见第 3.4 节。
- client-streaming：0/N 条请求都允许，handler 产生一个响应。Rust 客户端必须读到**唯一响应和最终成功 Trailer**才返回成功；零响应/多响应为协议错误，不能把第二条响应静默丢弃。
- server-streaming：正常客户端发送一个请求后 END_STREAM；服务端收到第一条请求即可调用 handler，不为了等客户端半关闭而阻塞首响应。零请求时返回错误；异常客户端额外请求在有限预算内处理并记录差异，不增设连接级停读。G0 以 Go 实际容错确定有限丢弃/拒绝边界。
- bidi：请求和响应可以独立推进。输入正常 EOF 仅表示对方半关闭，不表示取消；停止发送后仍能接收。
- server-streaming/bidi 客户端的标准 gRPC 方法 future 在**收到响应 Header 或 trailer-only 状态**后完成；发送 body 在等待 Header 期间也须被传输层驱动。不能强行复用 TTStream 的“发出 HEADER 即返回”语义。
- Kitex 服务端可能延迟响应 Header；提供文档与示例说明先送请求/先返回 Response 再生产消息的时序，避免双方等待 Header 的死锁。新 Rust handler 返回 `Response<Stream>` 即可开始 Header；不要求首条响应准备好。
- 请求 stream 自然结束/发送端 `close_send` 表示半关闭；显式 cancel、调用 future 被 Drop、客户端响应流提前 Drop 取消整个 RPC。服务端用户不再读取 request 不等于自动取消响应方向；底层保留控制事件与受限残余数据处理。
- 不复制 TTStream 的 sid、RST payload、`ttscp`、META、LongConn 模型到 gRPC。H2 的 stream id、流控、RST_STREAM、GOAWAY 由 H2 传输负责。

### 3.4 Unary 的 payload、IDL 和异常契约

unary 不只是“三种 streaming 各收发一次”。Kitex 的生成 unary 客户端传 Args/Result，服务端 `invokeStreamUnaryHandler` 也用 MethodInfo.NewArgs/NewResult；gRPC codec 直接编码这些对象。以 `Resp Echo(1: Req req) throws (1: Failure failure)` 为例：

```text
request payload  = struct EchoArgs   { field 1: Req req; STOP }
success payload  = struct EchoResult { field 0: Resp success; STOP }
throws payload   = struct EchoResult { field 1: Failure failure; STOP }
```

三条 payload 都位于 5 字节 gRPC prefix 之后，没有 message envelope；unary 返回声明异常时仍有一条合法 Result，不能替换成 `grpc-message` 或 TTStream 异常帧。单参数 struct、scalar/container 返回值均通过这层包装，不能从 IDL 参数数量推断使用 raw Req。

| IDL | 新 `thrift_grpc` 入口行为 |
| --- | --- |
| 无 streaming 注解 | 作为 unary；支持普通 Thrift 的零/一/多参数，scalar/struct/container/typedef 返回值、void 和声明 throws |
| `streaming.mode="unary"` | 同样作为 unary，线上使用 Args/Result；保留当前 Pilota 的单参数校验，其他合法类型/void/throws 按 unary 处理，不套用 streaming 的原始消息限制 |
| client/server/bidirectional | 保持原始 Req/Resp 与已有 streaming 签名限制；任何 service 的此类方法带 throws 仍导致整个构建失败 |
| oneway（任意注解状态） | 仅在新 gRPC 后端预检明确拒绝；不修改原 Thrift 后端或用“成功空响应”伪造其语义 |
| 混合/继承 service | 全量生成全部方法；一个 client/server、同一地址、同一 H2 连接池，按方法描述区分 UnaryArgsResult / StreamingRawStruct |

注解的参数限制需写入 G0 IDL golden 与迁移文档：**无注解 unary 支持零/多参数；显式 `streaming.mode="unary"` 必须恰好一个参数**。这在当前 Pilota 与固定 thriftgo v0.4.5/Kitex v0.16.4 上是共同限制，不能标为已确认的跨语言不对称。不要仅为选择 gRPC 给多参数方法补 unary 注解；保留无注解方法，在客户端显式选择 `transport.GRPC`。G0 分别记录无注解和显式 unary 的 0/1/2 参数生成结果、Go 普通/legacy/StreamX 入口及真实诊断；其他版本/定制生成器的行为另留版本证据，不泛化到所有历史 Go 版本。

零参数 unary 仍发送一个空 Args（含 STOP）；成功 void 仍返回一个空 Result（含 STOP），Rust 对外结果是 `()`。无响应 DATA + OK Trailer 不能解释为 void 成功；非 OK Trailer 则先作为远端错误处理，不被“缺响应”覆盖。

Rust 服务端默认先验证恰好一个请求及请求 END_STREAM 再调用 unary handler，零/多个请求返回 `Internal(13)`、不进 handler；对只发一条消息却不半关闭的调用，等待受调用 deadline/取消约束，不另加秒级隐式超时。这比 Kitex 当前“首个 RecvMsg 即可调用”的容错严格，是显式差异。server-streaming 保持第 3.3 节的首请求即调用规则，不能复用该 unary 提取器。

客户端对 unary/client-streaming 共用严格单响应提取器：先验证响应数与最终 status，再解包 unary Result。最终非 OK status 优先于 Result 内的 success/throws，不能提前暴露成功或声明异常。对端 zero/two responses、首响应后失败、void+trailer-only 都要独立验证。

发送端 Result 只允许 success 或一个声明异常；没有 throws 的非 void 方法必须有 success。接收端对“缺 success”“仅未知异常字段”“success 与异常并存/多个异常”的默认策略是返回 `Internal(13)` 并保留诊断，不默默构造默认成功；G0 记录 Go 生成客户端的实际默认值/异常优先级，作为畸形或跨版本 Result 的容错差异。合法 declared exception 的字段 ID、结构和结果类型必须与 Go 完全相符。

复用现有 Pilota Args/Result 的生成能力时，核查它是否在解码过程中提前合并/丢弃多字段信息；必要时增加新后端专用的 Result 解码状态或生成适配，不为了验证 unary Result 改坏原 Thrift runtime。`ApplicationException` 是框架异常，与 IDL `exception` 类型化返回是两回事。

## 4. Rust API 和模块设计

### 4.1 Codec 扩展保持现有 Protobuf 源码兼容

首选在 `volo-grpc` 内泛化现有编解码管道，复用 HTTP/2、压缩与 framing。设计草案如下，最终签名由 G0 的编译兼容样例冻结：

```rust
// 默认类型参数保持现有 RecvStream<T> 的含义。
pub struct RecvStream<T, D = DefaultDecoder<T>> { /* ... */ }
pub type ThriftRecvStream<T> = RecvStream<T, ThriftDecoder<T>>;

// 保留 encode(source, compression)、RecvStream::<T>::new(...) 原入口。
// 新增 encode_with_encoder(...)、with_decoder(...) / 带 options 的入口。
// ThriftEncoder<T> / ThriftDecoder<T> 直接使用 pilota::thrift::Message。
```

1. 沿用现有 Encoder/Decoder trait，不伪造 `pb::Message` 实现、不将 Thrift 包成 protobuf bytes 字段。Thrift 解码不额外要求 `T: Default`，普通消息不统一增加 `Clone`/`Sync`。
2. 类型定义、旧构造器、Debug/Unpin/Send 等条件按下表逐项冻结；增加默认类型参数本身并不能证明兼容。若泛化确实破坏旧接口，保留旧类型与新 `CodecRecvStream` 包装共享内部实现，或明确另开 breaking 版本，不能悄悄加约束。
3. 生成的 EntryMessage 以静态分派选择 codec。增加有默认实现的能力查询/带 options 入口，旧第三方 `SendEntryMessage` / `RecvEntryMessage` 实现保持可编译；同样核对 object-safety、方法解析与 trait bound。
4. **同一个 service 的 payload kind 必须同质**：全部 Thrift 或全部 Protobuf；同一 Thrift service 内可混合 unary/streaming，按方法描述区分 `UnaryArgsResult` / `StreamingRawStruct`。service 描述固定 payload kind/content-type，方法描述固定 wire path/RPC mode/包装方式。不同 payload kind 的 service 作为不同 NamedService 共存于同一显式 gRPC server，不需要连接嗅探。本轮不提供一个 service 内方法级 Thrift/Proto 混编选项；这是生成/API 范围约束，不能把它表述为 Rust enum 在类型系统中绝对无法表达异构 decoder。
5. 运行时 `thrift` feature 默认关闭：`volo-grpc/thrift`，同时控制 Thrift unary 和 streaming。codegen 选择独立于 runtime feature；缺 feature 的新生成代码给出清晰构建诊断，不回退 TTHeader unary 或 Protobuf。

G0 对 `decode.rs` 冻结以下具体改动与第三方兼容 fixture；不能只检查新类型定义能否编译：

| 位置 | 冻结要求 |
| --- | --- |
| `decoder` 字段与构造 | 字段改用 D；保留 `impl<T> RecvStream<T, DefaultDecoder<T>>::new` 的旧签名和无 Message 约束的构造能力；泛型路径新增 `with_decoder`，避免 new 重载/类型推断歧义 |
| `message` / `trailers` / `decode_chunk` / `Stream` | 当前 `trailers` 实际也在 `T: Message + Default` 的 impl 中，因为会 drain message；均改由 `D: Decoder<Item = T>`、解码错误转 Status 等必要条件约束，不把 Protobuf bound 搬给 Thrift |
| identity 与解压后的 decode | 两处 `DefaultDecoder::<T>::decode` 都替换为 `<D as Decoder>::decode`；使用非 Protobuf、带调用计数/跨消息状态的 decoder 分别运行 identity/gzip，防止只改字段而仍调用默认 codec |
| Unpin / Debug / Send | Debug 覆盖 `impl<T, D>` 且不新增 T/D: Debug；泛型 poll 路径先选 D: Unpin，手写 Unpin 的边界与之相符，默认 D 不增加 T: Unpin。若 G0 改用支持 !Unpin D 的投影方案，必须说明字段 pin 契约，不能机械推广原无条件 Unpin；Send 只取决于实际保存状态的必要 bound |
| 旧使用方式 | 独立下游 crate 编译 `RecvStream::<T>::new` / turbofish、类型推断、旧 encode、旧生成代码、自定义 EntryMessage；覆盖 type alias、alias re-export、持有该 alias 的 newtype，包括默认 decoder 下的 !Unpin 消息 |
| 第三方 codec | 实现现有 Decoder trait 的非 Protobuf 类型与独立错误类型（可转 Status），构造 `RecvStream<T, D>` 并调用 message/trailers/Stream；有状态 Encoder/Decoder 的创建数为每流一次，消息计数连续 |

`DefaultEncoder<T>` 当前仅包含 PhantomData，是零大小类型；循环内 `default()` 并不等于每消息堆分配。改造要求是 encoder 实例在消息循环外创建并复用状态，不为 codec 重建引入逐消息 Box/容器分配；payload、framing 与压缩缓冲的必要分配单独计量，不承诺整条发送路径零分配。

### 4.2 新生成入口与接口形状

提供 `volo_build::Builder::thrift_grpc()`，以及对应 workspace/config builder。底层组合 Thrift parser、Pilota Thrift 数据后端和新的 gRPC service 生成器。旧 `thrift()`、`protobuf()` 保持原语义。

配置保留 `protocol: thrift` 表示 IDL 语言，在现有 `codegen_option.config` 增加 `rpc_transport: grpc` 选择新后端；同一生成 entry 内 transport 必须一致，冲突给出诊断。不通过新增 `IdlProtocol` 枚举值混淆 IDL 与传输，避免破坏既有 exhaustive match；CLI 初始化/更新也必须保存该配置。

新入口支持纯 unary、纯 streaming 和混合 service（含继承方法），无注解与显式 `streaming.mode="unary"` 都生成 gRPC unary 方法。仍生成全部 service，不新增选择 service 的能力；合法 unary throws 保留类型化语义，任意 service 的三种 streaming 方法带 throws 仍让整个构建失败，不按业务是否使用筛选。仅包含数据类型的 include 可正常使用，oneway 的新入口诊断发生在 emit 之前。

每个 service 在所选生成模块内只有一个 FooClient/FooClientBuilder 和 FooServer；其 unary/streaming 方法共享发现、配置、handler 状态、目标地址和 H2 连接池。生成的 gRPC builder 不要求应用手配 codec；不增加第二地址或自动 fallback，也不让现有 TTStream FooClient 隐式改走 gRPC。需要两套协议适配的应用显式使用不同生成模块，公共消息类型应复用而非重复定义。

以下为接口形状示意，不是当前已存在的 API：

| 方法模式 | 客户端方法 | 服务端 handler |
| --- | --- | --- |
| unary，无 throws | `Request<FooMethodRequest> -> Response<Ret>` | `Request<FooMethodRequest> -> Response<Ret>` |
| unary，有 throws | `Request<FooMethodRequest> -> Response<Result<Ret, FooMethodException>>` | `Request<FooMethodRequest> -> Response<Result<Ret, FooMethodException>>` |
| client | `Request<S<Req>> -> Response<Resp>` | `Request<ThriftRecvStream<Req>> -> Response<Resp>` |
| server | `Request<Req> -> Response<ThriftRecvStream<Resp>>` | `Request<Req> -> Response<thrift::SendStream<Resp>>` |
| bidi | `Request<S<Req>> -> Response<ThriftRecvStream<Resp>>` | `Request<ThriftRecvStream<Req>> -> Response<thrift::SendStream<Resp>>` |

`thrift::SendStream<T>` 为 owned message stream 与预编码有界 channel 的单层发送源；`SendStream::from_stream` 适配普通 `Stream<Item=Result<T,Status>>`。`thrift::IntoStreamingRequest` 同时接受普通 Rust stream（无需 Sync）、带 metadata 的 Request 和 channel 接收端；旧 Protobuf 的同名 root trait 及约束保持。`thrift::channel` 返回通用 Sender 与 SendStream，客户端和服务端使用同一 helper；按消息数及编码字节有界，所有 sender Drop 后半关闭。`send(&T)` 在返回前同步编码，future 只持有 bytes/发送器/permit，使用 Rust 2024 精确捕获避免跨 crate 再次带入借用。压缩仍由同一 framing 管道处理，不能为支持借用发送伪造消息或解码重编码。

所有方法为异步、外层返回 `Result<_, Status>`。`FooMethodRequest` 是具有原参数名称/字段 ID 的生成请求类型，零参数为空结构体，多参数也只对应一个 gRPC message；统一使用该形态，避免方法参数数量变化时改变 metadata 的承载方式。unary 的 `Ret` 是 IDL 返回类型，void 为 `()`；有 throws 时内层 Rust `Result` 表示声明异常，外层 Status 表示传输/框架/业务终态，server 与 client 使用同一结果模型。Args/Result 的线上包装由生成适配器完成，应用不手写 success 字段；不依赖 `volo-thrift::MaybeException`。

**声明异常的应用层类型在 G0 冻结：只要 throws 非空，统一使用方法级 enum，单异常也不简化成异常结构体或其类型别名。** `FooMethodException` 指 Pilota 已生成的异常枚举，gRPC 后端通过 `method.exceptions` 获取并复用同一类型，不生成第二份。无 throws 保持 `Response<Ret>`；void + throws 使用 `Response<Result<(), FooMethodException>>`。

以 service Foo 的 `Resp Echo(1: Req req) throws (1: ExA a, 2: ExB b)` 为例，接口形状如下（示意，不是新增运行时代码）：

```rust
// 复用 Pilota 生成类型；variant 按 throws 字段名 a/b 命名。
pub enum FooEchoException {
    A(ExA), // MethodResult field 1
    B(ExB), // MethodResult field 2
}

// handler 的完整返回类型为：
// Result<Response<Result<Resp, FooEchoException>>, Status>
// handler 已得到 ex_a / ex_b / resp 时分别返回：
// Ok(Response::new(Err(FooEchoException::A(ex_a))))
// Ok(Response::new(Err(FooEchoException::B(ex_b))))
// Ok(Response::new(Ok(resp)))
// 框架/传输错误仍为外层 Err(Status::internal(...))。
```

client 在读完最终成功 Trailer 后返回相同 enum，应用可按 `Err(FooEchoException::A(ex_a))` 或 B 分支匹配。服务端适配器把选中的 variant 写入其原始 field ID，其他 Result 分支不写，随后发 `grpc-status=0`；异常枚举本身不作为额外嵌套结构体或 message envelope 发送。非 OK Trailer 仍优先于已解码的异常，不能落入内层 enum。

类型名、variant 的大小写/关键字/重名处理沿用 Pilota 命名结果；variant 按声明字段名区分，不能只按 ExA/ExB 的类型名生成，也不依赖按载荷类型自动 From 转换，以免同类型多字段时歧义。继承/include 从方法定义解析已有异常类型路径，不能按每个派生 client 再生成一份；各 variant 保存现有 Pilota 类型，不统一额外 Box/Clone 或强加新的 trait bound。

单异常时仍为 `FooEchoException::A(ExA)`，从一个 throws 增为两个时 handler 的返回类型名称/嵌套形状不变，已有返回 A 的代码也不必换包装。**这不代表新增异常完全源码兼容**：枚举新增 variant 仍可能破坏用户的穷尽 match；从无 throws 改为有 throws 也会改变签名。复用现有 enum，不为了新入口全局添加 `#[non_exhaustive]` 或 Unknown variant；未知 wire 异常仍按 3.4 返回外层 Status。G0 编译 fixture 必须分别记录返回签名稳定性与 exhaustive match 的演进影响。

客户端普通入口沿用 gRPC `IntoRequest` / `IntoStreamingRequest` 习惯；内部 body 是可失败 Stream，另外提供受限发送 helper 或明确的 fallible 入口，确保生产任务失败会取消请求而不是转换成正常 EOF。unary 使用一次性 body，不为单消息调用创建多余的 channel/producer task。服务端流式方法允许返回惰性响应流，不强制每流另起 producer task。

需要交互式发送时提供可选有界 channel helper，单个 sender 默认不 Clone、`&mut self` 排序，明确半关闭、cancel、drop 与消息预算。`send(&T)` 沿用 TTStream 已冻结的借用发送契约：**普通同步函数在返回前完成同步编码，再返回只持有 owned bytes/发送器的 Future**。函数返回后允许修改或 Drop 原消息；不捕获消息借用，不为此增加 T: Sync/Clone。用 `Send + !Sync` 消息编译/运行 fixture 验证，不能写成借用消息跨 await 的 `async fn`。取消待发送 future 不能部分入队；编码完成不等于远端业务收到。

文档提供有界 channel helper 与 `try_join!`/`select!` 并发驱动发送、接收的示例，并区分建流 Header 等待与应用层背压死锁。单条小消息 `send().await` 后 `recv().await` 本身不是错误；风险是双方先发送超过队列/H2 窗口容量的数据、都等发送完才读取，形成循环等待。回归用例使用小窗口、超过总缓冲的数据量和有界超时，先证明串行样例无法推进，再证明并发 drain/取消能完成；不能把无限增大 buffer 当修复。client 的调用 future 与请求生产者也必须并发驱动，不能先填满 channel 再启动 RPC。

### 4.3 Context、hook 与完整生命周期

新增 `volo_grpc::streaming::StreamContext`，通过 Request/Response extensions 与流 handle 暴露同一共享状态。生成的 handler 保持 gRPC 风格 `handler(&self, Request<T>)`，从 request 获取可 clone 的 context；hook 显式接收该 context。这样不修改现有 Protobuf handler 签名，也不把 `&mut ServerContext` 借用到整个响应流结束。

context 至少保存权威 deadline、取消通知、调用身份、方法模式、metainfo、终态与统计。`Service::call` 返回后仍有效，但不保留可被复用的栈上 context 引用。

unary 使用同一生命周期模型：请求已完整读取且 handler 正在执行时，RST/断连仍能取消 handler 和受管理下游任务；客户端等待最终 Trailer 的阶段也受 deadline 管理。其用户方法不暴露 Stream，但不能退回只等待 Response Header 的短生命周期。正常声明异常（内层 Err）与 gRPC 成功终态分开观测，避免把业务失败记录成传输失败或重复计数。

- 建流阶段继续 motore `Service` / 现有 layer，复用发现、负载均衡及配置。消息/终态阶段采用单层 hook，覆盖 open/header/send/recv/trailer/io-error/finish；不逐层包装流产生泛型增长，不使用 TTStream 专属 `on_meta` 事件。
- 无 hook 使用静态 Noop 路径，无额外 dyn dispatch/Vec 分配/逐消息锁；生命周期 context 自身成本单独测量，不把整个运行时称为零开销。
- 框架负责 handler future、请求 body poll、响应 body poll 的 task-local scope；惰性响应流在 handler 返回后仍获得同一上下文。不能只包住 handler 的第一次 await。
- 裸 `tokio::spawn` 不自动继承作用域或取消。提供 `cx.spawn_scoped` 一类受管理任务入口，并说明手动 `cx.scope` 只传递上下文；裸任务须自行监听 `cx.cancelled()` 和建立资源归属。
- cancel/断连/deadline 使 handler future、待发送/待接收任务和受管理子任务退出，已开始的下游 RPC 使用自身 H2 reset/Drop 级联取消。先返回响应 Header、request 已半关闭、业务不再 poll request 都不能让取消失效。
- wire terminal、handler/producer 完成、资源释放是不同事件。最终状态只发布一次；在全部框架受管理任务退出前保留生命周期登记，避免重现“结束回包后丢失 handler 取消句柄”。
- 响应 stream 被提前 Drop、发送流错误、server early return、connection EOF、panic、RST 与正常 Trailer 竞态都有确定状态；消费后丢弃单个消息值不会取消 RPC，正常 request EOF 不标记 canceled。

G0 同时冻结 hook 的**注册、安装和持有位置**，下游治理/观测适配依赖此契约，不等到 G3 才决定：

| 边界 | 接线与生命周期 |
| --- | --- |
| 注册 | 在 client builder 与 `server::ServiceBuilder` 提供同名 `.stream_hook(factory)` 扩展（拟新增 API）；生成 builder 转发，应用适配层在构建时组合治理/观测 factory。factory 为每次 RPC 建立独立状态，多个 hook 的顺序在构建时固定，不逐消息分配列表 |
| client 建流 | 在现有 client Timeout 层入口读取预算、建立共享 StreamContext/deadline 与 hook 所属状态，放入 request extensions，再调用内层发现/LB/用户 layer；on_open 先于内层调用。layer 可读取/补充 context，失败也走该 RPC 的唯一 finish；不能等待 client Service 返回才注册 |
| server 建流 | MetaService 创建调用 context；Router 选中 service 后安装该 service 注册的 factory，在内容类型校验、`T::from_body` 和 ServiceBuilder 应用 layer 之前调用 on_open。描述与同一 context 经 request extensions 交给生成适配器/handler；路由前错误归现有全局观测，不虚构已选 service hook |
| 消息/终态 | CodecService、client transport 将同一 hook 状态交给收发 codec、body 与取消/drop guard；hyper 在 motore call 返回后继续 poll Body 时仍可发出 send/recv/trailer/io-error/finish。持有 owned context/guard，不保存 `&mut ClientContext/ServerContext` 或依赖 task-local 查找 hook |
| 状态提交 | 编码完成/完整消息解码成功的 size 与 wire 提交分开定义；Header/Trailer 合并 context 后才回调，finish 看最终状态且恰好一次。hook 为同步、有界回调，禁止持锁跨 await；发送/接收可并发，顺序保证只限各方向和终态屏障 |
| 旧 layer 适配 | 发现/LB 等建流 Service 保持原栈顺序；应用适配层将 span、并发许可、实例统计等需要全流持有的 guard 转交生命周期状态，terminal hook 负责收尾。不能假定所有旧 layer 的 call 返回天然等于流结束，也不能重复记录原 call 和 hook 的成功/释放 |
| 无注册路径 | 构建期默认 Noop；不创建 hook task、Vec、逐消息锁或额外 dyn callback。G0 原型检验已有 BoxBody/Stream 擦除边界能保留 Noop 路径，公共 Sender/RecvStream 不按每层 hook 增长泛型；不满足时须在冻结前明确成本/替代方案，不能仅写“零开销” |

G0 fixture 至少覆盖：自定义 motore layer 注册/读取状态；on_open 拒绝；handler 返回 Response 后才产生消息/Trailer；Header 返回后取消；正常/错误/Drop/panic 结束一次；无 hook 与启用组合 hook 的类型和分配对照。生命周期登记和 hook 状态直到全部受管理任务退出才释放；on_finish 的逻辑终态并不允许提前销毁取消句柄。

**G0 必须先验证底层能力。** 用当前 hyper/h2 构造“handler 一直 pending、请求已 END_STREAM、客户端 RST/断连”“返回 Response 后 producer pending”“窗口耗尽后取消”原型，证明无需业务继续 recv 即可获知取消。优先在 service future/body 与连接归属上绑定 drop guard 和取消句柄；若现有抽象缺少必要 reset 信号，G0 给出最小 H2 接线方案、代码影响和兼容测试后再冻结。不能以无界后台 drain、全连接停读、轮询业务 recv 或只记录 token 代替可靠取消。

### 4.4 Deadline、流控与内存

- gRPC 使用 `grpc-timeout`。接收时建立单调时钟绝对 deadline；下游传播剩余预算，并取上游 deadline、调用配置、服务端配置的最小值。出站发送 Header 前再扣除排队/发现/建连时间，禁止每跳重新使用完整 Duration。
- 改造现有 `client/layer/timeout.rs` 的 header 写入点：由它建立权威绝对 deadline，移走提前写完整 Duration 的行为，统一由出站提交边界序列化一次剩余预算，替换已有 `grpc-timeout` 而非追加第二个值。G0 用慢 layer/慢 connector/池内等待验证真实 wire；hyper legacy 的 `.ready()` 不等于已完成内部排队/建连，若缺少最终提交回调，需先冻结取得连接后生成 Header 的最小传输接线，不能宣称仅在 `.call(req)` 前改头已覆盖隐藏等待。
- deadline 覆盖建流、handler、全部消息和最终 Trailer。现有只围绕 `Service::call` 的 timeout 不能作为完整流 deadline；Thrift 模式使用共享生命周期定时器，避免多个 timer 得出不同终态。
- 解析/序列化覆盖 H/M/S/m/u/n、最多 8 位数字、舍入、0、过期、溢出和非法输入。格式错误有可观测错误，不 panic；没有 deadline 时不擅自添加秒级全流超时。
- 建流时预算已耗尽：客户端本地 deadline <= now 时不发请求，返回 DeadlineExceeded；服务端收到 `grpc-timeout: 0n`，或本地队列耗尽接收时创建的预算时，在 decode/handler 前返回 Thrift trailer-only `DeadlineExceeded(4)`，handler 调用数为零。不能依赖先 poll inner 再 poll timer 的竞态。`grpc-timeout` 是相对时长，不能从正值头部反推发送方绝对 deadline 已在网络途中到期。
- HTTP/2 stream/connection window 是传输流控；应用消息队列另外有消息数/字节上限，受控发送不会无界缓存。一个慢流不能阻塞同连接其他流与控制事件。
- 冻结可配置的单消息编码/接收/解压大小、每流 buffer、共享内存预算、最大活跃流及 header list 上限；在 `reserve`/解压之前校验长度。初始建议 Thrift 单消息 16 MiB，最终数值与窗口/队列预算一起在 G0 依据 Go 样本冻结，记录这是 Rust 的资源保护增强。
- 单消息允许值必须与每流预算一致；大消息分片接收可逐步释放 H2 capacity，不能因应用只在“完整消息入队后”归还窗口造成死锁。缓冲已满时仍须有 reset/deadline 的独立唤醒机制。
- 尽量复用 Bytes/LinkedBytes，避免为每条消息构造 metadata map。小切片持有大 buffer 时复制脱离，阈值用分配/retained memory 实测决定；压缩路径限制解压后大小及持续 CPU 占用。
- 使用现有 H2 写入/调度能力，不另建 TTStream 的 socket writer 和 sid map。不按消息起任务；每流必要生命周期任务计数、栈/队列/定时器开销必须进入性能报告。

### 4.5 H2 连接池、keepalive 与有限排空

无预算/无 hook 的既有 Protobuf 调用保留 hyper-util legacy 池；Thrift、deadline 和受管理生命周期调用使用可控 h2 提交池，以真实 SETTINGS 名额及最终 Header 提交扣除预算。新池默认每 endpoint 最多 4 条连接、128 条排队请求、4096 个 endpoint，空闲 90 秒回收；客户端共享实例，不按 RPC 建连。G0 冻结每个源站/authority 的复用、最大并发流与等待/扩容预算；G6 分别测长 bidi 活跃但尚有 stream slot、达到对端 `SETTINGS_MAX_CONCURRENT_STREAMS`、大量短 RPC 与大量长流并存。记录实际 socket/建连数、短 RPC p50/p99/排队、池高水位及结束后回收；未满时不按 RPC 建连，满时遵守冻结的有界排队或扩容策略，不能要求所有负载永远只用一条连接，也不能无界扩容。

keepalive 基线：Volo client/server interval 默认 None，client while-idle=false；Kitex server enforcement 的 MinTime 默认 5 分钟，活跃但无 DATA 的 bidi 仍受 ping 频率约束。风险发生在显式启用不兼容的 ping 配置时，不宣称默认参数必然杀流。I28 分默认禁用、协商兼容、故意违规三组：空闲跨多个配置周期后恢复收发；违规超过 strikes 后记录 `GOAWAY/ENHANCE_YOUR_CALM` 与 `too_many_pings`、失败归因及连接淘汰，不重放业务。无活跃流的 PermitWithoutStream/while-idle 另测。加速 fixture 可缩短双方测试参数，但报告保留真实默认值，不能把短周期测试冒充生产配置实测。

服务端目前收到 shutdown 后只触发连接 `graceful_shutdown()` 并无限等 `tx.closed()`，没有到期清退。本轮增加显式 drain deadline、连接/调用任务登记：停止 accept/拒绝新流，旧流在期限内完成；到期取消 handler/producer/受管理下游，reset 或关闭仍占用的连接，等待回收并有最终有界的强制退出路径。G0 冻结可配置默认值、竞态和本地时间门槛。验收须包括 request 已 END_STREAM 而 handler pending、无限 bidi/producer pending，断言 server run future 和任务按期退出，不只验证新流被拒。

## 5. 实施 checklist

G0～G6 由本仓库实施；C1～C4 是其中可独立 review 的公共修复单元，随 Thrift over gRPC 提交交付。TTHeader streaming 与 Thrift over gRPC 共用既有分支，但各自保留独立提交，不跨功能 squash。只有代码/验证证据齐全才能勾选；SKIPPED/NOT_EXECUTED/DEFERRED_BY_USER 不计 PASS。下列 G 阶段完整保留拆分前的 57 条任务，使用稳定编号便于交接和核对。

### 公共前置 PR C1～C4 的独立 checklist

这些是 G0/G3 内的 PR 划分，优先进入公共基线，随后由 Thrift 接入复用；不是四套新状态机。R1～R4 的最简复现、预期/实际推断与命令见第 10.1 节。各项先在未开启 thrift feature 的 Protobuf/raw peer 上验证。

#### C1：状态解析健壮性

- [x] **C1-01** 移除 details-bin base64 解码 expect，保持 Option<Status> 签名；非法输入返回 Internal，保留有界原始状态诊断，不能吞错成成功。
- [x] **C1-02** 初始 trailer-only/最终 Trailer、code=0/非零、合法带/不带 padding、非法 base64 与多 RPC 隔离均验证；保留修复前 panic 复现。
- [x] **C1-03** 默认/无 thrift 构建及 Protobuf 回归、nightly fmt/clippy 无新增问题；提交可独立 review 的公共修复，G4 只补 Thrift 回归。

#### C2：入站已过期请求

- [x] **C2-01** 在 handler 前检查接收时建立的权威 deadline，覆盖 0n 和队列内过期；禁止先 poll inner 再检查已到期计时器。
- [x] **C2-02** Protobuf/raw peer 断言 handler=0、无 DATA、trailer-only DeadlineExceeded；正预算/无 deadline 正常工作。
- [x] **C2-03** 共享 deadline 原语、配置优先级和竞态有回归；不为 Thrift 单独绕过公共 timeout，也不增加重复 timer/终态。
- [x] **C2-04** 普通 cargo check、适用 feature/target、nightly fmt/clippy 和针对性测试通过，兼容变化单列。

#### C3：出站剩余预算

- [x] **C3-01** 移走旧 Timeout 层提前写完整 Duration 的行为；出站最终提交时只有一个 grpc-timeout，使用剩余预算，耗尽不发请求。
- [x] **C3-02** G0 用慢 layer/connector/H2 slot 等待验证真实 Header；不能以 legacy client ready() 代表已经完成内部建连/排队。
- [x] **C3-03** Protobuf 三跳、舍入/溢出/0/无预算、终态竞态通过。三节点原始头验证见 `protobuf-relay-with-and-without-budget.log`，可在关闭 Thrift feature 时运行。
- [x] **C3-04** 根 check、相关 feature/target、nightly fmt/clippy 无新增问题；保存修复前完整 Duration 与修复后递减预算的原始证据。

#### C4：有限 drain

- [x] **C4-01** 新增冻结的 drain deadline 和连接/调用任务登记；停止 accept/拒收新流，允许期限内完成，替换无限 tx.closed 等待。
- [x] **C4-02** 到期取消 handler/producer/受管理下游，reset/关闭残留连接并有界回收；server run future 有最终退出期限。
- [x] **C4-03** Protobuf request 已 END_STREAM 而 handler pending、producer pending、无限 bidi、正常完成/新流拒绝均验证；观察实际任务退出。
- [x] **C4-04** 默认值与公开配置/迁移说明经 G0 冻结，正常调用兼容；根 check、nightly fmt/clippy、针对性测试有独立报告。


### G0：冻结 wire/API/生命周期及兼容门槛

主要位置：`tests/interop/kitex-streaming`、新增设计与编译 fixture。

- [x] **G0-01** 固定上游 Kitex v0.16.4/thriftgo，生成纯 unary、三模式 streaming 和混合 IDL；分别记录普通 Client、legacy StreamClient、StreamX 生成方式/支持的方法与调用入口，客户端显式选 `transport.GRPC`，防止 unary 降为 TTHeader 或 streaming 默认走 TTStream。
- [x] **G0-02** 记录 content-type、path/ServiceInfo、prefix/raw payload、Header 时序、压缩、status/biz/details、模式容错和严格服务路由 oracle；namespace/include/继承 golden 标记 PackageName 所属文件，unknown service=20 与 unknown method=1 分开固定。
- [x] **G0-03** 冻结 unary Args/Result、void、声明异常与 Status/biz 优先级，实测默认 `invokeStreamUnaryHandler` 兼容路径；记录“首请求即调用”与 Rust 严格单请求提取、畸形 Result 容错的差异，不把改变 Go 默认中间件开关作为基本互通前提。
- [x] **G0-04** 冻结 4.2 的统一方法级异常 enum：0/1/多个 throws、void、每个 variant 的 handler 返回与 client 匹配、field ID 映射、include/继承路径均有 API 编译样例；单异常增为多异常保留返回类型形状，新增 variant 对穷尽 match 的影响明确记录，复用 Pilota 类型身份。
- [x] **G0-05** IDL golden 记录无注解/显式 unary × 0/1/2 参数；当前 Go/Rust 的显式 unary 都拒绝非单参数，不把共同限制记成跨语言差异。Go 各生成入口与版本、诊断及迁移建议可追溯。
- [x] **G0-06** 用最小泛化原型按 4.1 的逐项表冻结全部 impl/字段/两个 decode 调用点与 Unpin 约束；独立下游 fixture 编译 turbofish `RecvStream::<T>`、alias/re-export/newtype、旧构造器/encode、自定义 EntryMessage、非 Protobuf Decoder 和老生成产物，原型运行有状态 codec 的 identity/gzip 测试，G1 在正式实现复跑。
- [x] **G0-07** 冻结 service payload 同质性、路由后/codec 前的 content-type 校验、HTTP status/内容类型的错误回包传递，以及默认 HTTP/2-only 与显式 accept_http1 分支的兼容口径。
- [x] **G0-08** 冻结 4.3 的 client/ServiceBuilder hook factory 注册、motore layer 接线、extensions/body/codec/guard 的所有权与事件顺序；完成 call 返回后 hook 仍有效及 Noop 成本的原型，下游治理/观测接线直接依赖此项。`hook-allocation/report.json` 的 9 组实测区分 context、注册和消息开销；未注册 hook 的增量分配为零，消息与终态事件无新增分配。
- [x] **G0-09** 原型验证 4.4 的唯一 timeout 写入点及发现/池等待/建连后的预算扣减；明确 legacy client 的最终提交边界，不以 `.ready()` 代表取得可用 H2 stream。
- [x] **G0-10** 执行第 4.3 节取消原型，固定 hyper/h2 reset、service future/body Drop 和 connection teardown 接线；原型未过不进入“仅接 codec 即可交付”的开发路径。
- [x] **G0-11** 冻结资源预算、错误分类、模式边界、有限 drain/Drop 契约、连接池等待/扩容与 keepalive 配置/测试时间门槛，区分与 Go 一致和 Rust 增强项。
- [x] **G0-12** 建立当前 Protobuf、TTStream、本地 default cargo check、fmt/clippy 基线，保存已有失败；源码/lock/toolchain/命令有来源记录。
- [x] **G0-13** 为 R1～R4 建立无需 Thrift feature 的 Protobuf/raw peer 复现，按第 8 节 C1～C4 组织为可独立 review 的公共修复单元。本轮随独立的 Thrift over gRPC 提交交付，不并入 TTHeader streaming 提交，也不另开前置 PR；功能与独立无 Thrift 回归要求不变。

完成条件：wire 契约、公开 API、取消技术方案、资源门槛均可执行验证，下游适配可据此开发，无未归属的关键扩展点。

### G1：通用 codec、Thrift Binary 和 H2 内容类型

依赖：G0、P0/P1；集成基线包含公共修复 C1。主要位置：`volo-grpc/src/codec/{mod,encode,decode}.rs`、`message.rs`、`transport/client.rs`、`server/{router,service,meta}.rs`、`status.rs`。

- [x] **G1-01** 泛化 encode/RecvStream 字段、构造/方法/Stream/Debug/Unpin 全部 impl，保留原默认入口；identity 与解压分支实际调用 D 的 Decoder trait。增加 feature-gated Thrift encoder/decoder，复用 Pilota Binary，执行 G0 下游 fixture。
- [x] **G1-02** 所有消息编码和解码通过同一 framing/压缩管道，encoder 在消息循环外创建、decoder 随流持有；计数 codec 证明每流一次构造且连续状态不丢。分开统计 payload/buffer 分配，不能把原 ZST default 构造报成逐消息堆分配；prefix 长度 checked conversion，超限返回 Status 而非 assert/panic。
- [x] **G1-03** EntryMessage/方法描述将 codec、RPC mode、UnaryArgsResult/StreamingRawStruct、content-type 送达传输，确保早期错误、trailer-only 和普通响应均正确；同连接交替 unary/streaming 不共享错误的 payload 解释状态。
- [x] **G1-04** 在 Router 解析描述后、CodecService 的 `T::from_body` 之前校验 Thrift content-type；实现真正 HTTP 415 与 MetaService 错误回包策略。HTTP/2 与显式 accept_http1 的 HTTP/1.1 负例断言 decoder/handler 未调用；旧 Protobuf 路由不新增全局校验回归。
- [x] **G1-05** 处理 partial prefix/body、同 frame 多消息、消息终止、缺少 Trailer、错误后再次 poll 和 fused stream；终态及 Trailer 可读取，错误不导致 metadata 丢失。
- [x] **G1-06** 实现消息/解压预算；不让对端声明长度直接触发巨额 reserve。无压缩为默认互通必测，gzip 为启用后的互通必测；zlib/zstd 等只按两端实际注册情况声明能力。
- [x] **G1-07** 未知 grpc-encoding、缺失 encoding 却压缩、非法 flag、解压失败/超限、空结构体与零字节 payload 有测试；Go 更宽松的非法输入容错单列差异。

完成条件：字节级 framing 和 Thrift codec 测试、content-type 请求/响应/错误路径及 Protobuf 默认路径通过，尚不能据此宣称完整网络互通完成。

### G2：Thrift gRPC service 代码生成

依赖：G0、P0、G1；主要位置：新增 `volo-build/src/thrift_grpc_backend.rs`，抽取 gRPC service 模板共用部分，接入 `lib.rs`、`config_builder.rs`、`workspace.rs` 和 CLI。

- [x] **G2-01** 新后端组合 Pilota Thrift 数据生成与 gRPC service 模板，不改旧后端含义；Builder/config/workspace/CLI 选择一致。
- [x] **G2-02** 四模式 client/handler、Send/Recv entry enum、wire path、NamedService 和方法描述完整生成；unary 使用 Args/Result 包装，streaming 使用原始 Req/Resp，混合服务只生成一个 client/server。
- [x] **G2-03** 实现 unary FooMethodRequest、Result 的 success/声明异常转换与 void，支持无注解/显式 unary；非空 throws 直接复用方法级异常 enum，单/多异常保持同一形状，逐 variant 验证 handler/client 返回与原始 field ID。单 struct 参数不做隐式 raw 编码。
- [x] **G2-04** unary 严格单请求+END_STREAM，unary/client-streaming 共用严格单响应+Trailer 提取器；server-streaming 读取首请求即可调用 handler，不等待请求 EOS，额外请求与 Kitex 一样忽略，不套用 unary 的等 EOF/严格基数适配。raw peer 同时验证请求保持打开及额外 DATA 两种情况。
- [x] **G2-05** 合法 unary throws、任意 service 的 streaming throws 整体失败、oneway 拒绝、纯 unary/混合/继承、多参数/标量/容器/void/include、多 service、原始方法名、自定义命名、split/workspace、Arc/Box 编译 fixture 完整。
- [x] **G2-06** 新生成 crate 只依赖 `volo-grpc`/Pilota，不为 payload 编码引入 `volo-thrift` 的传输/TTStream 运行时。
- [x] **G2-07** 同一 IDL 同时生成 TTStream 与 thrift_grpc service 模块，include 的共享结构体只生成一份 Pilota 消息定义；编译 fixture 直接在两个入口间传递同一类型（无转换/重编码），覆盖 Arc/Box、跨文件继承与 workspace，不允许重复生成导致类型身份不同。
- [x] **G2-08** 生成 service 级 payload 描述；Thrift 的 unary/streaming 混合允许，方法级 Thrift/Proto 混编无配置入口。分别生成的 Thrift/Proto NamedService 同 server 共存且正确选择 codec/内容类型。
- [x] **G2-09** 保留旧 Protobuf 生成快照与已生成源码编译样例，两个版本生成器与运行时的最低兼容矩阵明确。基线生成器重产的 36 个文件与旧快照一致，并用当前 runtime 编译。见 `target/thrift-grpc/frozen-protobuf-compile-report.json`。

完成条件：四模式及混合 service 端到端 Rust 自回环和静态编译检查通过；所有拒绝项在生成阶段报明原因，无空 handler 占位或静默漏生成 service。

### G3：全流 lifecycle、deadline 和取消

依赖：公共生命周期子集只依赖 G0，按 C2～C4 独立前置 PR 实现；Thrift 接入部分依赖 G1 和对应公共修复，可与 G2 后半段并行开发。主要位置：新 `volo-grpc/src/streaming/`、body、transport、server/service、timeout layers。

- [x] **G3-01** 将请求/响应 body、handler/producer、connection 所属关系绑定同一 StreamContext；response Header 返回后保留全流 deadline、取消与终态。
- [x] **G3-02** 接通 handler 不读消息时的 RST/断连取消；半关闭后取消、handler 返回前/后、发送受流控阻塞、任务 spawn 归属的回归均通过。
- [x] **G3-03** unary 请求读取完后 handler pending、response DATA 已发而 Trailer pending 时，cancel/断连/deadline 仍生效；与长流共享连接时，取消 unary 只影响当前 RPC。
- [x] **G3-04** 请求 EOF、发送异常、客户端 response Drop 与服务端 request Drop 分别处理；不会取消正常的 server-streaming 响应，也不泄漏被放弃的 RPC。
- [x] **G3-05** 改造现有 client timeout 写入点并统一最终提交时序列化；绝对 deadline、剩余预算、三跳递减、配置优先级与边界解析生效，wire 上恰一个 grpc-timeout。慢 layer/connector/池等待被扣除；预算耗尽不发请求，挂起生产/消费均可终止。
- [x] **G3-06** 服务端 `0n` 或队列内过期在 handler 前 trailer-only DeadlineExceeded；不得先 poll handler 再检查计时器。fixture 验证 handler 调用数为零、无 DATA、内容类型和终态正确。
- [x] **G3-07** 按 G0 实现两层 Service/hook 注册和 body/codec/终态接线，Noop 路径与受管理 spawn/context scope 可用；call 返回后事件仍生效，终态、统计、配额释放 exactly once。
- [x] **G3-08** 流创建后禁止自动重放和 backup request；核查 Hyper 客户端默认 retry/canceled request 行为，禁用无法证明未提交的重试，GOAWAY 不导致已发送业务重复执行。
- [x] **G3-09** graceful shutdown 使用 H2 GOAWAY/drain：新增 drain deadline 与连接/任务登记，替换无限 `tx.closed()` 等待；拒收新流、旧流限期排空，到期取消并有界回收 handler/producer/下游与连接，server run future 按期退出。取消单流不关闭健康共享连接，断连接管全体流。
- [x] **G3-10** 接入并文档化 H2 keepalive/ping enforcement 的兼容配置，维持原禁用默认值；加速的兼容周期、违规 GOAWAY 与无活跃流选项已有实际结果，不自动重放。生产分钟级周期未执行。

完成条件：本地最小复现和三跳取消测试证明 handler/下游任务按时释放。必须观测任务退出/Drop/下游 reset，不能只断言 context 标志已变。

### G4：Metadata、业务状态与可扩展配置

- [x] **G4-01** 标准 metainfo 的 HTTP prefix 导入/导出由 Volo 负责：transit/transient 固定一跳，persistent 跨多跳，不实现 Kitex bugfix 兼容 flag；Header 与 Trailer 的 backward metadata 分开处理，同 key 的终态值覆盖初始值，缺失值不清空既有值。不承诺 Go 自动提供其没有写出的字段，并记录具体 Go API 成功/错误终态的实际处理时机。
- [x] **G4-02** 自定义 ASCII、多值和 `-bin` metadata round trip；不将二进制 metadata 视为 unreachable，不将 HTTP key 大小写规整误当业务 key 丢失。
- [x] **G4-03** Kitex biz helper 保留 Status 和成功、失败、trailer-only 的 metadata。覆盖 percent-encoded message、原始 UTF-8 JSON、转义、坏 JSON/UTF-8 和超过 H2 header limit 的输入。默认和显式上限的实测结果见实施状态。
- [x] **G4-04** 复用前置 C1 对 `Status::from_header_map` 的公共修复，追加 Thrift 的初始/最终 Trailer 和 code=0/非零回归；合法带/不带 padding、其他 binary metadata 保真，不重复实现 base64 错误处理，也不以开启 Thrift feature 作为避免 panic 的条件。
- [x] **G4-05** 固定 Go unknown service 的 wire 20 → Rust Unknown(2)、unknown method 的 wire 1 → Rust Cancelled(1)，原值可诊断；保留远端 status 与本地主动取消的来源差异，不按该码推导重试或用户取消。
- [x] **G4-06** unary 声明异常保留在 Result 与 typed 返回值中，非 OK Trailer 优先处理；不将类型化 throws 转成 biz-status，也不把 gRPC code=OK 等同于业务结果成功。
- [x] **G4-07** success Trailer 可携带用户 trailer metadata；惰性 producer 结束时生成最终状态，`Status::Ok` 与 helper 业务状态的保留字段冲突明确拒绝。
- [x] **G4-08** 暴露 codec limits、H2 window/concurrency、stream deadline/hook 配置；现有 Protobuf 的默认配置和自定义 layer 源码兼容。

完成条件：第 3 节 wire/错误矩阵有 Rust 回归与 Go fixture，传输状态和业务状态不混为一个计数。

### G5：公共 Go 互操作工具与报告

依赖：G1～G4、P1；使用既有 `tests/interop/kitex-streaming` 中的 Python 包、Go module 和进程管理。

- [x] **G5-01** 新增显式 `grpc-thrift-*` suite，包含 unary/explicit-unary/client/server/bidi、纯与混合 service 的 IDL 和 Rust fixture，复用基础设施；不误运行旧 TTStream unary/LongConn pool 场景来代替 gRPC 验收。
- [x] **G5-02** 上游 profile 完成第 6 节基础及边界矩阵；fixture 从真实 socket 收发、记录实际协议/内容类型，不拿 raw codec 自回环替代 Go 互通。
- [x] **G5-03** 增加纯 H2/raw peer 支持恶意分片、错 subtype、trailer-only、异常单响应、坏 details-bin/base64、非法 biz-extra、0n、取消/GOAWAY；另有显式 accept_http1 的 HTTP/1.1 415 fixture。真实 Kitex 与构造 peer 的结果明确区分，按第 10 节提供可独立筛选的最简复现。
- [x] **G5-04** runner 使用 readiness、有界超时、进程组清理和独立输出目录；命令失败/二进制不存在/环境异常不能 PASS，不自动修改 golden。
- [x] **G5-05** 本地候选 manifest 与分用例报告记录基线和远端 SHA、实际 lock、Go module/sum、生成器和二进制 hash、suite 版本、输入与预期/实际；可公开索引不包含部署专用 profile 或本机路径。

完成条件：Volo 开源双向互通报告完整，供下游适配消费；不依赖下游正式发布。

### G6：性能、兼容复核与本地候选交付

- [x] **G6-01** 执行第 7 节共享运行时与 codec 性能/资源矩阵；长 bidi 未满/占满并发额度期间测短 RPC 的实际建连数、p99、排队与池峰值，取消/超限后按冻结预算回收。identity/gzip 分开报告复制与分配，不能合并为零拷贝结论。
- [x] **G6-02** 执行 Volo root 普通 cargo check、feature 矩阵、nightly fmt/clippy、相关 tests 和旧 Protobuf/TTStream 回归；保存新增与基线 lint 的区分。
- [x] **G6-03** 消费下游候选互通反馈 D1，修正公共问题；依赖候选而非正式版，避免与下游最终交付形成循环依赖。公开反馈见 [D1](thrift-grpc-downstream-feedback.md)；公共复现已修正，性能与来源门禁仍单独保留。
- [x] **G6-04** 提供兼容矩阵、配置/API 文档、候选版本及源码/lock 来源；只准备本地材料，不发布 crate/tag。见[候选交接](thrift-grpc-candidate.md)。
- [x] **G6-05** 文档包含 channel helper 与并发 send/recv、有限 drain、keepalive 和池参数示例；I28 空闲恢复/违规退出有运行证据，应用循环等待示例有失败对照及正确并发版本。`application-flow-current.log` 实际验证同任务停滞/取消后的并发完成；真实 Go keepalive 加速用例见 `network-boxed-current`。

完成条件：公共实现可在本地独立构建、验收与 review，下游使用相同候选锁完成集成。

## 6. Go 互操作和边界验收矩阵

固定上游 profile 为 Kitex v0.16.4/thriftgo v0.4.5。每个适用用例记录方向、IDL 模式、TCP/UDS、Go API 形态、源版本和终态。基础为两方向 × unary/explicit-unary/client/server/bidi 五类 × 两种 transport = **20 个组合**，不是全部用例数。所有方法显式选 transport.GRPC，不能只选 GRPCStreaming 后假定普通 unary 也走 gRPC。

I 编号沿用共同验收索引；下表是 Volo 负责的适用场景，不以编号连续与否判断遗漏。P0/P1 的数据/生成证据由 Pilota 提供，网络验证由 G5 负责；每行都要在 manifest 有具体 case 和状态，未运行不得记 PASS。

| ID | 用例 | 关键断言 | 责任阶段 |
| --- | --- | --- | --- |
| I01 | 两方向×五类 IDL 模式×TCP/UDS；流式 0/1/N，unary 正常 1/1 | 请求/响应一致；unary/client-streaming 恰一响应+成功 Trailer；server-streaming 正常恰一请求 | G5 |
| I02 | 5 字节 prefix 分片、payload 跨 H2 frame、多个消息合帧 | 不丢消息；unary Args/Result 与 streaming raw 按方法选择，不加 message envelope；消息不依赖 frame 边界 | G1/G5 |
| I03 | empty struct `[0x00]`、0 字节、未知字段、嵌套/大容器、类型演进 | 显式包含 Rust 发 → Kitex fast codec 收空结构体；0 字节为独立负例；语义及差异明确，无 OOM/panic | P1/G1/G5 |
| I04 | raw content-type/path、namespace/无 namespace/include/继承/rename | 使用真实 Go ServiceInfo，记录 PackageName 归属文件；正确 handler，错误 subtype 不触发 decoder/handler | G0/G2/G5 |
| I05 | unknown service、unknown method 两个分支、重复路由、Go fallback | 无 fallback 的 Go wire 20 → Rust Unknown(2)、wire 1 → Cancelled(1) 分开验证；Rust 路由错误 12；不误记本地主动取消 | G0/G4/G5 |
| I06 | delayed Header、并发驱动 body、双端先写满后才读的反例 | Header 等待可推进；串行大批量写反例在有界时间检测阻塞，并发 send/recv 正例成功，取消释放；小消息先发后收允许 | G0/G2/G3/G5 |
| I07 | 半关闭/空请求流、额外 server-stream 请求 | 正常 EOF 不等于取消，剩余请求处理有界；模式容错有说明 | G2/G3/G5 |
| I08 | unary/client-streaming 零/两个响应、首响应后失败 Trailer | 唯一响应检查；后续错误不能被首响应成功/声明异常覆盖 | G2/G5 |
| I09 | trailer-only、若干消息后错误、缺少 status、截断连接 | 保留 metadata/status；无成功 Trailer 不报告成功 EOF | G1/G4/G5 |
| I10 | biz code/message/extra、应用错误、details、非 ASCII message/extra | grpc-message percent-encoding 与 biz-extra 原始 UTF-8 JSON 分开；中文 key/value、引号/反斜杠/转义控制字符/非法输入/大小边界；Go/Rust 保真，无 EXCEPTION DATA | G4/G5 |
| I11 | ASCII、多值、binary metadata，Header/Trailer backward 冲突 | 分阶段导入，终态值按约定覆盖，缺失不清空；保留允许的原始字段，无 panic/串值，固定 Go 实际行为 | G4 |
| I12 | 三跳 transit/transient 单跳、persistent 多跳 | Rust 固定单跳 transient + 多跳 persistent；不实现或验收 Kitex 兼容 flag，Go Legacy 泄漏仅列迁移差异 | G4 |
| I13 | client/server deadline、0n、队列内过期、三跳及慢 connector/池等待 | 剩余预算递减且 Header 唯一；本地已过期不发请求，接收已过期 trailer-only 4 且 handler=0；Header 后仍超时，无 deadline 不注入 | G0/G3 |
| I14 | 主动 cancel/RST、客户端进程退出/连接断开；含 unary 请求已读完 | 即使 handler 不再 recv，handler/受管理任务/下游 RPC 在期限内退出；不连带关闭其他 RPC | G0/G3 |
| I15 | request 已半关闭、Header 已发/终态竞态、response Drop | 取消仍有效；结束一次；正常半关闭和 drain 不误记取消 | G3/G5 |
| I16 | producer pending/错误/panic、受管理 spawn 与裸 spawn 示例 | 实际退出/drop 被观测；裸任务的显式责任被说明，不声称强制终止任意用户任务 | G3 |
| I17 | 慢长流、窗口耗尽；长 bidi 未满/占满 stream slot 期间短 RPC | 未满可复用；满额排队/扩容有界，实际建连数/连接峰值、短 RPC p99/排队按冻结门槛；控制/取消仍推进，结束后池按期回收 | G0/G3/G6 |
| I18 | identity/gzip、未知 encoding、flag 错误、解压炸弹 | 合法压缩互通；非法/超限有界失败，不改变 Protobuf 默认压缩 | G1/G5 |
| I19 | drain/GOAWAY、新流拒绝、旧 handler/producer 永久 pending | 期限内完成者成功，到期取消其余流/下游并回收；server run future 按期返回，不能无限等 tx.closed；不重放业务 | G3/G5 |
| I21 | 旧 protobuf 源码+旧生成代码、无 feature、TTStream | 旧编译/API/行为无新增回归；新生成缺 feature 清晰失败 | G0/G2/G6 |
| I22 | 无注解/显式 unary × 0/1/2 参数、纯/混合/继承 service、throws/oneway 编译矩阵 | 固定基线 Go/Rust 显式 unary 均要求单参数，无注解允许零/多参数；unary throws 成功，streaming throws 整体失败；新入口 oneway 拒绝 | G0/P0/G2 |
| I23 | unary 零/一/多参数、非 1 参数字段 ID、scalar/container/typedef/void | Go/Rust Args/Result golden 一致；单 struct 参数仍有 Args 包装；void 是一个空 Result，不是零响应 | P1/G2/G5 |
| I24 | unary 单/多 throws 的每个 variant、void、include/继承、1→N API 演进、错误/biz/畸形 Result | 统一方法级 enum，单异常不退化；handler 可构造各 variant、client 匹配相同类型和 field ID；合法 Result + OK，最终错误优先；签名与穷尽 match 的兼容性分别记录 | G0/G2/G4 |
| I25 | 同一 client/server/地址/H2 连接交替并发 unary 与三模式长流 | 共享 handler/发现/池配置；正确选 Args/Result 或 raw；metadata、deadline、取消隔离；慢长流不阻塞正常 unary | G2/G3/G5 |
| I26 | unary 零/两个请求、仅一请求但不 END_STREAM、正常半关闭后取消 | 严格请求基数，不执行零/多请求 handler；未半关闭受 deadline/取消管理；与 Go 首请求即调用差异有 oracle | G0/G2/G3/G5 |
| I27 | 独立下游 crate 的旧 RecvStream/新 Decoder/Encoder 用法 | turbofish、alias/re-export/newtype、Unpin/Debug/Send、旧构造器保持；identity/gzip 真实调用自定义 decoder，有状态 codec 每流一次构造 | G0/G1/G6 |
| I28 | idle bidi 跨多个 keepalive 周期、恢复收发、无活跃流、故意违规 ping | 默认禁用行为不变；兼容设置存活，违规捕获 ENHANCE_YOUR_CALM/too_many_pings 并回收；实际配置、周期/strikes、是否加速明确记录 | G0/G3/G5 |
| I29 | 已知 Thrift 路由错 subtype/非 gRPC/missing CT，HTTP/2 与显式 HTTP/1 | subtype 错回 12，非 gRPC 回真实 HTTP 415；decoder/handler=0；MetaService 不覆盖 CT/status；默认 HTTP/1 禁用和旧 Proto 路由保持基线 | G0/G1/G5 |
| I30 | grpc-status-details-bin=%%% 等坏 base64；初始/最终 Trailer，code=0/非零 | 返回 Internal 不 panic、不产生成功 EOF；合法带/不带 padding 保真；前置 C1 覆盖 Proto，G4/G5 追加 Thrift | G0/C1/G4/G5/G6 |
| I31 | hook 注册、motore layer、call 返回后的 Body poll、无注册路径 | 单一 context/事件顺序正确；消息/终态回调仍生效，guard 仅释放一次；Noop 的类型/分配/调用成本有证据 | G0/G3/G6 |
| I32 | 同 IDL 的 TTStream/thrift_grpc 两入口、共享 include；Proto/Thrift service 共存 | 一份消息定义、直接传递同一类型；不同 NamedService 选对 codec，无方法级混编入口，旧生成类型身份不变 | G2/G6 |

在仓库根构建当前 Go oracle，再使用配套 Rust release 二进制执行网络验收。

```bash
go -C tests/interop/kitex-streaming/go build -mod=readonly -buildvcs=false \
  -o "$PWD/target/thrift-grpc/perf-binaries/fallback-go" ./cmd/oracle
PYTHONPATH=tests/interop/kitex-streaming/python python3 -m kitex_streaming_interop \
  --profile upstream --protocol grpc-thrift \
  --suite grpc-thrift-basic,grpc-thrift-gzip,grpc-thrift-legacy,grpc-thrift-metadata-values,grpc-thrift-boundaries,grpc-thrift-lifecycle,grpc-thrift-multihop,grpc-thrift-keepalive \
  --go-bin target/thrift-grpc/perf-binaries/fallback-go \
  --rust-bin target/thrift-grpc/perf-binaries/zlib-header-rust \
  --transport tcp,uds --direction both --out target/thrift-grpc/reproduced-upstream
```

- [x] **G-INTEROP-01** G5 为上表每个适用 I 场景提供具体 IDL/输入/预期、可筛选 case 和实际命令，含 20 个基础组合。
- [x] **G-INTEROP-02** unary/streaming/throws/void、Header/Trailer、metadata/biz/非法 details、deadline/cancel/drain/keepalive 与共享池均有真实 Go 或明确标记的 raw peer 证据。
- [x] **G-INTEROP-03** Go oracle 与 Rust fixture 使用独立 readiness、有界超时和进程组清理；保留命令失败/资源失败，不自动更新 golden 或阈值。
- [x] **G-INTEROP-04** 结果包含最简复现、版本/lock/module/sum/hash、输入、预期/实际、原因和执行状态；D1 仅接收可公开反馈，公共源码/日志不带部署专用信息。

使用仓库 target/ 或显式证据目录，记录空间预算与清理归属，不默认向 /tmp 堆积产物。Pilota 的 Cargo 测试只读 golden；本 runner 统一提供 Go 动态校验和更新，不新增重复工具链。

## 7. 性能、资源与 Rust 质量门槛

性能基准分两类：同一 Protobuf/TTStream workload 的变更前后回归，以及 Thrift gRPC 的新能力报告。不能用 Thrift 与 Protobuf 不同 payload 的绝对吞吐比较代替回归。

| 维度 | 必测内容 |
| --- | --- |
| 运行组合 | Rust↔Rust、Go→Rust、Rust→Go；原生 Volo 按 codec/生命周期/观测分层比较；TCP/UDS |
| 负载 | unary 与三种 streaming，64 B/1 KiB/64 KiB/1 MiB，1/32/256 活跃 RPC；参数超出冻结预算时单列预期拒绝，不混入成功吞吐 |
| 生命周期 | 大量 unary、长 bidi、client-streaming 聚合、server-streaming 慢读及混合负载；H2 单连接复用、并发流上限等待、连接冷建/热复用/重连，idle bidi 跨 keepalive 周期后继续收发 |
| 连接池专项 | 同一 client/目标/authority，长流占用少量/全部并发额度时持续短 unary；测实际建连数、池峰值/扩容或等待、短 RPC p50/p99、取消后的复用与 TTL 回收；区分 TCP/UDS、冷/热连接 |
| unary 专项 | Args/Result 包装及 throws/void 的编码成本、严格单响应+Trailer 的延迟、无额外 channel/producer task 的分配；与同连接慢 streaming 的隔离 |
| 配置 | 无压缩/gzip、hooks off/on、观测 off/on、不同 H2 window、有界队列饱和与恢复 |
| 编码/复制 | identity 与 gzip 单独报告：codec 构造数、payload/buffer 分配、线性化复制字节、压缩输出分配、retained memory；零大小 DefaultEncoder 构造不计作一次堆分配 |
| 指标 | 吞吐、header/首消息/尾消息/全流 p50/p99、CPU、分配、retained memory、队列高水位、流控等待、FD/连接/任务、回收时间 |
| 资源 | 正常结束、cancel、reset、deadline、Drop、drain、反复连接中断；handler/下游任务退出证据 |

identity 可复用 LinkedBytes、FastStr 和 Bytes 的 payload 切片。gzip、zlib 和 zstd 会把输入片段交给同一个压缩流，按实际输出增长缓冲。旧版本的整份输入拼接保留在基线报告中。分开记录 identity 的切片复用、压缩输出的分配及各自的保留内存。

冻结门槛：同机同 lock/toolchain、固定数据集，至少 3 次独立测量并报告噪声。建议旧 Protobuf 吞吐下降不超过 5%、p99 上升不超过 10%，出现超限须解释并修正或显式复核，不能用单次更好样本覆盖。最终参数在 G0 基线报告中固定，实施后不为过验收修改阈值。

资源采用此前确认的**归属、容量边界、按期回收**口径：逐项说明 H2 连接/活跃流/队列/定时器/受管理任务的 owner 和释放点。600 秒混合负载后，在配置的关闭/池 TTL 加固定调度余量内回到基线允许范围；期间峰值不得超过声明预算。保留原始时间序列和失败，不使用单独 FD 斜率或“最后是零”代替完整证明。


开发阶段执行以下本仓库检查；thrift feature、新回归目标在实现后才存在：

```bash
cargo check
cargo +nightly fmt --all -- --check
cargo check -p volo-grpc --all-targets --features thrift
cargo check -p volo-grpc --all-targets --no-default-features --features thrift
cargo +nightly clippy -p volo-grpc -p volo-build --all-targets \
  --features volo-grpc/thrift
# 原有 literal-unwrap / items-after-test-module 诊断单列；不能称为 -D warnings 全绿。
# 新增 fixture 可独立执行严格检查。
cargo +nightly clippy -p thrift-grpc-codegen --all-targets -- -D warnings
cargo test -p volo-grpc --features thrift
# C1～C4 不依赖 Thrift feature，使用实际拆分的公共回归目标
cargo test -p volo-grpc --no-default-features \
  --test public_status --test public_deadline --test final_submission \
  --test public_cancellation
```

- [x] **G-QUALITY-01** 根普通 cargo check 无需 path patch/临时环境；default/no-default、旧生成 crate、新生成 crate、all-targets/doc tests、自定义 EntryMessage/Decoder、TLS 与必要 ShmIPC 组合编译完成。
- [x] **G-QUALITY-02** nightly fmt/clippy 按既有 lint/feature/target 执行，无新增问题；基线失败/不可运行原因保留，不添加全局 allow 掩盖新增问题。
- [x] **G-QUALITY-03** 第 7 节性能每组至少 3 次，旧 Protobuf 吞吐/p99 回归与新 Thrift 结果分开，identity/gzip/hooks off/on 成本分别呈现。
- [x] **G-QUALITY-04** 600 秒原生候选资源记录包含容量上限与 TTL 内释放；原始 FAIL、时间序列、池/任务高水位与回收日志可复核。受管理任务归属和释放点另有生命周期 fixture。
- [x] **G-QUALITY-05** 只读计划修改不填写任何运行 PASS；没有执行的 feature、场景与平台在报告列为 NOT_EXECUTED 或 DEFERRED_BY_USER。

## 8. 交付顺序、版本与 PR

```text
G0（协议/API/取消/最终提交原型）
  ├─ C1 公共 status 修复 → G1/G4 复用
  ├─ C2/C3/C4 公共 deadline/预算/drain → G3 复用
  ├─ Pilota P0/P1 → G1/G2
  ├─ G1 codec → G2 codegen
  └─ G3 lifecycle → G4 metadata/status
G1/G2/G3/G4 → G5 公共 Go 互通
G5 + D1 下游候选反馈 → G6 本地候选复核
```

C1 可在 G0 较早实施；C2～C4 依赖 G0 的相关原型，不能先等它们完成才开始 G0。相关公共修复先进入公共基线，再叠加 Thrift 接入。C2～C4 的公共子集不依赖 G1；Thrift 生命周期接入才依赖 G1。其他模块可以并行开发，不能把 C3 的隐藏排队或 C4 的取消回收当一行修补。

其余 PR 按 codec、codegen/统一异常 enum、Thrift lifecycle/status 接入、互通、性能/迁移材料拆分；每个 PR 保留实现、针对性回归和已知差异。共同异常类型来自 Pilota，单一类型身份由 G2 fixture 证明。D1 消费候选而非正式发布，G6 无需读取任何私有仓库或生产环境。

| 包 | 候选版本方案（未确认号段，不执行发布） |
| --- | --- |
| volo-build | 当前 0.13.0 候选线上准备 alpha.2；既有生成入口不变 |
| volo-grpc | 完全新增且源码兼容时可准备 0.12.3-alpha.1；公开 trait/type 发生不兼容时按 0.x 升 minor 至 0.13.0 候选并附迁移表 |
| volo 核心 | 无必要不改；若需公共任务/网络接线，按实际兼容性单列，不为了凑模块扩展范围 |
| Pilota 依赖 | 消费 P0/P1 候选与锁，实际无需源码变化时记录复用结论 |

Cargo.toml 仅用具名 branch（例如现有 feat/ttheader-streaming-20261105），不写 commit id；Cargo.lock/manifest 固定解析 SHA。旧 Protobuf/TTStream 的 breaking change 需单列影响与替代方案，不藏在 feature 后；新 thrift_grpc handler 与旧 TTStream handler 是显式不同 API。

## 9. 本仓库最终完成 checklist

- [x] **G-DONE-01** G0～G6 与 C1～C4 所有有效项有实现/验证证据；依赖 P0/P1/D1 的交付状态明确，不把下游正式发布作为依赖。
- [x] **G-DONE-02** 公共修复在未开启 thrift 的 Protobuf 上独立通过，Thrift 接入复用同一实现。见 `profile-gates/protobuf-zlib-header-tests.log` 和当前网络套件。
- [x] **G-DONE-03** 四模式/混合 service、统一单/多异常 enum、共享消息、旧 API/source/feature 兼容有编译和网络证据。
- [x] **G-DONE-04** 上游 Go 20 个基础组合及第 6 节全部适用边界通过；未知错误码差异、严格基数、Legacy/single-hop、非法输入增强明确记录。
- [x] **G-DONE-05** handler 不再 recv、请求已半关闭、Header 已返回、窗口耗尽时取消仍有实际退出证据；有限 drain 和最终提交预算没有技术空洞。
- [x] **G-DONE-06** 普通 check、nightly fmt/clippy 无新增问题，性能/容量/回收达冻结门槛，失败与原始来源可追溯。
- [x] **G-DONE-07** API/配置/迁移、并发 send/recv、keepalive/池/drain 示例和本地候选材料完整；未执行真实部署、ShmIPC 运行或实际发布；远端 CI 恢复正常触发，历史未执行记录不计 PASS。

## 10. 本轮复核结论与最简验证说明

以下编号保留计划评审阶段的判断；实际实现与执行结果见[实施状态](thrift-streaming-over-grpc-status.md)。表中的“源码确认”本身不算运行 PASS，新增测试/API 的实际证据按后面的映射及实施状态核对。功能风险与性能/文档建议分别记录，不把尚未测量的池膨胀、默认 keepalive 断流写成既成缺陷。

| 意见 | 核对结论与性质 | 计划落点 |
| --- | --- | --- |
| 1 | 泛化遗漏风险成立；当前 trailers 在有 Message + Default 约束的 impl 内，两个 decode 分支都显式调 DefaultDecoder。属于 G0 兼容性门禁，尚无新的泛化实现可判定失败 | 4.1 逐项表、G0/G1、I27 |
| 2 | 注册/所有权缺口成立；现有 ServiceBuilder 的 call 不能代替后续 Body poll 事件。属于实现前必须冻结的扩展点 | 4.3 注册表、G0/G3、I31 |
| 3 | 同质性需要明确；不是 Rust enum 本身不能表达异构 decoder。采用 service 同质、NamedService 并存的范围约束 | 4.1、G2、I32 |
| 4 | 当前确实未校验 CT，且正常响应会被 MetaService 覆盖；这是新 Thrift 接入缺口。accept_http1 默认 false，HTTP/1 415 只测显式开启分支 | 3.1、G0/G1、I29 |
| 5 | 契约方向已有，补为与 TTStream 相同的同步 encode，并用 !Sync 消息证明无跨 await 借用 | 4.2、I27 |
| 6 | identity/gzip 分项已有；确认 concat 的额外输入复制，修正“协议必然要求整份拷贝”的过强表述。属于性能说明/优化边界 | 第 7 节、G6 |
| 7 | 当前逐消息构造 Encoder 属实；默认 Encoder 是 ZST，不能据此证明逐消息堆分配。新增有状态 codec 需要每流复用 | 4.1、G1、I27 |
| 8 | 测试缺口成立；还没有证据确认现有池会泄漏或必然膨胀。属需实测的容量/性能风险 | 4.5、G6、I17 |
| 9 | Kitex 有 ping strikes/GOAWAY enforcement；风险取决于显式配置，Volo 默认 interval=None。属条件风险，非默认必现故障 | 4.5、G3、I28 |
| 10 | 补示例有必要；单任务先发一条小消息再读不必然死锁，需有界缓冲/窗口导致的循环等待才能复现 | 4.2、G6、I06 |
| 11 | 两个错误映射经源码确认，需独立 oracle；非标准 20 与标准但语义误导的 1 不应合并描述 | 3.2、G0/G4、I05 |
| 12 | JSON/HTTP 字节边界测试缺口成立；Go 正常编码会转义控制字符，不能预设它发送裸 CR/LF/NUL。HTTP header 合法不代表 to_str 可接受中文 | 3.2、G4、I10 |
| 13 | 保留四项已有方向并补显式验收：Trailer 覆盖、空 STOP 的 Rust→Go、PkgInfo 所属文件、双入口单一消息身份；Go Trailer 读取触发条件按 API 记录 | P1、G0/G2/G4、I03/I04/I11/I32 |
| 14 | 现有 timeout layer 先 poll inner，零预算不能保证阻止 handler；源码确认的正确性风险。相对 grpc-timeout 不能表达远端绝对过期时间 | 4.4、G3、I13；复现 R2 |
| 15 | 现有 shutdown 没有 deadline，pending 连接使 tx.closed 无限等；源码确认的生命周期缺口 | 4.5、G3、I19；复现 R4 |
| 16 | 现有客户端在内层排队/发现前写完整 Duration，未扣减；源码确认的传播偏差。需移走旧写点并解决 hyper 内部等待 | 4.4、G0/G3、I13；复现 R3 |
| 17 | 非法 base64 可达 expect panic；源码确认的功能/健壮性缺陷，公共路径修复不能只藏在 Thrift feature 后 | 3.2、前置 C1、G4/G5、I30；复现 R1 |

### 10.1 公共缺陷的实际复现与回归入口

2026-11-11 更新：C1～C4 已实现并实际执行下列测试，不再使用规划时不存在的 `grpc_plan_regressions` 目标。完整运行结果与仍未完成的门禁见 [实施状态](thrift-streaming-over-grpc-status.md)。本地候选依赖需要显式 `--config target/thrift-grpc/candidate.toml`；这不代表无覆盖的远端分支已通过。

| 问题 | 最简输入、预期与原结果 | 实际回归命令及证据 |
| --- | --- | --- |
| R1 非法 details-bin | `grpc-status: 13`、`grpc-status-details-bin: %%%`；预期解析成错误而不 panic。原 `expect` panic，红例已执行 | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test public_status`；`c1-before.log` / `c1-after.log`，另测初始/最终 Trailer、code=0、合法 padding 与同连接后续 RPC |
| R2 建流即过期 | `grpc-timeout: 0n`，或接收后排队至预算耗尽；预期 handler=0、无 DATA、trailer-only 4。原先 poll handler 后才检查计时器，红例已执行 | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test public_deadline`；`c2-before.log` / `c2-after.log` |
| R3 预算不扣排队 | peer 并发额=1、首流占用 150ms、第二流预算 1s；预期仅写一次剩余预算。原线上仍为 1s，实际应约余 847ms，红例已执行 | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test final_submission`；`c3-slot-before.log` / `c3-slot-controlled.log`，`c3-handshake-current.log` 追加前置 layer/慢握手/握手中到期且未提交 Header |
| R4 无限 drain | 请求半关闭后 handler pending，或 Header 后 producer pending；配置短 drain 后触发 shutdown，预期按期退出且受管理任务实际 Drop。旧源码无限等连接结束的路径已确认；未对旧树执行新增 drain fixture，不冒称红例 | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test public_cancellation`；`c4-public-tests.log`，另测 server run future Drop、零窗口与断连 |
| 终态被 NO_ERROR reset 覆盖 | peer 先 flush 终态 Header，再 reset 未半关闭的请求方向；预期保留真实 code=4。原返回 Internal，修复前稳定红例已执行 | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test peer_no_error_reset`；`no-error-reset-flushed-before.log` / `no-error-reset-final.log`，覆盖成功/失败/缺失 Trailer、CANCEL，不将缺 status 当成功 |

每个测试自带有界超时和任务/进程归属。历史日志保留原失败；后续源码不追溯旧二进制的 PASS。C1～C4 的无 Thrift feature 回归另有实际记录，不把公共修复绑到新 payload。

### 10.2 设计契约的实际验证归属

| 场景 | 当前实现/证据与限制 |
| --- | --- |
| I27 codec 泛化 | `volo-grpc/tests/codec_compat.rs` 实测默认 turbofish、alias/newtype、自定义 Decoder、identity/gzip 状态连续和旧入口；全部 decode 路径调用 D |
| I31 全流 hook | hook factory 在 builder 注册，context/body 保持生命周期；公开取消与 flow_control 测试验证 call 返回后事件、双 hook 转发和真实任务退出 |
| I29 内容类型 | `thrift_content_type` 测试在路由后、解码前验证 HTTP/2 subtype 和显式 HTTP/1.1 415；旧 Proto 行为保留 |
| I05 未知目标 | 实际 raw/生成 peer 固定 Go service=20、method=1，Rust 归一化=2/1并保存原码；Rust 未知路由为12。Go 显式兜底的未知 service/method、已知路由共6项通过。重复注册由生成服务的启动前断言覆盖 |
| I10 JSON 业务字段 | 真实双向业务错误覆盖中文、引号、反斜杠和转义控制字符；raw peer 分开覆盖坏 JSON/UTF-8/超限 |
| I17 长流与短 RPC | 既有 managed_pool 测试覆盖有界扩容、全额排队、预算耗尽不提交；延迟/三次样本与完整负载性能门禁以实施状态为准，不用功能测试代替吞吐报告 |
| I28 keepalive | 真实 Go 的加速合法/违规 PING 和原生未 ACK 用例已运行；生产默认的分钟周期未运行，不能冒称完成 |
| I06 并发 send/recv | 使用文档提供 channel + try_join 示例，真实 Go 延迟 Header 已验证。`application_flow` 以4096条消息/1KiB窗口实际复现先写后读停滞，并在取消后用同一client并发完成全部消息 |
| I25 混合调用 | `unary_progresses_on_the_same_connection_as_three_active_streams` 验证同一条实际 H2 连接上的 unary 与三模式活跃流。小窗口阻塞 server-stream 时 unary 返回，取消该流后 client-stream/bidi继续成功。不同流的 deadline 独立 |
| I03/I04/I11/I32 | STOP 双向、namespace/include/继承、Header/Trailer backward metadata、共享消息与外部 crate 分别落到 golden/codegen/网络 fixture；完整组合仍逐项核对，不以生成文本相似证明类型身份 |

### 10.3 声明异常、IDL 边界与交付

已复用 Pilota 方法级异常 enum，单/多异常统一，void 成功值为 `()`；实际编译和 Go 双向结果逐 variant 校验。0→1 throws 改变返回值形状，新增 variant 影响穷尽 match。

固定 thriftgo v0.4.5/Kitex v0.16.4 与 Rust 均拒绝显式 unary 的非单参数签名；无注解 unary 的零/多参数合法。`thrift_grpc_ir` 和提交的 IDL matrix 保存实际诊断，不再将其标成未执行的源码推断。所有 service 继续整体校验，不新增选择性生成服务的选项。

C1～C4 按公共能力组织为独立 review 单元，在现有开发分支交付；不另开分支，不发布 crate/tag 或合并主干。未通过的性能门禁保留原始 FAIL，未执行的平台/部署项目保持用户延期状态。

最终按 I 编号筛选的输入、fixture、命令和预期见[验证与复现索引](thrift-grpc-verification.md)。`source-delivery-tests.json` 记录使用真实 Pilota 分支依赖的公共无 Thrift、Thrift/zstd、生成及 TTStream 回归；不以早期 path 覆盖的通过替代普通根 check。

G6 执行补充：`phase-latency-go-representative` 补齐Go客户端四模式的Header/首尾消息/终态采样，每仓16组48次；与Rust客户端32组96次分别保存。`codec-profile-completion.json`含24组streaming编码和78组Args/Result/异常/void编码微基准，`queue-profile-completion.json`含6组饱和/等待/恢复测量。两者是独立组件测量，不冒充整次unary RPC的分配或网络流控数据。

G-DONE-06 的Protobuf性能结论采用冻结政策允许的显式复核：`perf-protobuf-explicit-review.json`纳入所有相同二进制/时长/CPU/阈值的A/B样本，24配置均达原门槛；未删掉原完整矩阵22/24或其FAIL，不将单次绿样本当结论。Thrift完整576组和600秒容量/回收分别有独立通过报告。
