# Thrift over gRPC 实施状态

2026-11-11，当前分支 `feat/ttheader-streaming-20261105`。功能和双向互通已实现；性能与最终依赖交付按下文分别验收。开发分支源码按 Pilota→Volo→下游交付，不发布 crate/tag、不合并主干、不部署；2026-11-11 起恢复远端 CI。

## 当前实现与证据

| 模块 | 实现与实际验证 |
| --- | --- |
| 公共 C1～C4 | 坏 details-bin 不 panic；入站接收时冻结 deadline，0n/排队过期不进 handler；实际 H2 slot/HEADERS 提交前扣除 layer/建连/等待预算；默认30秒有限 drain，run future Drop 回收实际连接/调用 |
| Codec | `RecvStream<T,D=DefaultDecoder<T>>` 全部 impl/构造器/Debug/Unpin 泛化，identity/解压均调用 D；有状态 codec 每流构造一次；旧 EntryMessage/turbofish/alias/newtype/!Unpin/!Sync 编译回归 |
| Thrift 与生成 | Binary raw streaming、unary Args/Result、void、统一单/多 throws enum；四模式/混合/继承、wire 名、Arc/Box、CLI/config/workspace；共享消息和异常类型不重复定义 |
| 路由 | service payload 同质；Thrift/Protobuf NamedService 共存；路由后、decode 前校验 Thrift subtype，显式 HTTP/1 415 负例；原 Protobuf 内容类型行为保留 |
| 生命周期 | 单层 hook factory、motore 建流、StreamScope、受管理子任务；Header 返回后继续取消/终态/观测；不重放已提交调用；池/队列/消息/解压/深度预算明确 |
| metainfo | 本地 transient/transit 一跳、persistent 多跳；upstream transient 不再导出；无 Legacy/bugfix flag。Header/Trailer backward 分开，终态覆盖初始非空值 |

| 本地原始证据（均在 `target/thrift-grpc/`） | 结果与范围 |
| --- | --- |
| `grpc-tests-delivery-final.log` | 当前 runtime 151项测试 +84 doctests PASS，1 doctest ignored |
| `c3-handshake-current.log` | 无 Thrift feature：慢 layer、慢 H2 handshake、预算耗尽零请求提交及 slot 等待通过 |
| `grpc-protobuf-only-pool-final.log` | 关闭所有默认 feature 的136项测试、84 doctests PASS，1个ignored；公共修复独立于Thrift |
| `grpc-tls-shmipc-compile-final.log` | 无默认feature + Thrift/rustls/volo-shmipc all-targets编译；没有执行ShmIPC运行 |
| `g6-ttstream-tests-current.log` | 39项 TTStream filter 回归；其余89项未计入该数字 |
| `network-routing-current/summary.json` | 当前 release 两方向 TCP/UDS 共 282 项通过。含 basic/gzip/legacy 各 4 组、metadata 4 组、raw 202 项、生命周期 28 项、三跳 32 项和 keepalive 4 项。使用同一 Rust 二进制和补充 Go 路由 fixture 的 oracle |
| `profile-gates/generated-network-candidate.log` | 4 项生成服务用例通过。同一 H2 连接上三个流模式同时活跃时 unary 返回，取消受流控的 server-stream 后另两条流继续完成。另验收重复注册拒绝和双传输类型身份 |
| `network-delivery-final/summary.json` | 压缩和 header limit 改动前的 release 274 项通过 |
| `pool-latency-validated.log` | 6项 pool 回归；TCP/UDS各3次，单长流期间各64次短 RPC，满池实际排队约73ms；取消后复用，200ms idle后450ms触发重建 |
| `application-flow-current.log` | 真实H2：4096消息/1KiB窗口下，同任务先发后收停滞；取消后同一client并发收发全部完成 |
| `shared-wrapper-types-delivery.log` | TTStream与gRPC共用include/继承的消息与异常；同一`Arc<Node>`内含递归`Box<Node>`，两个模块直接赋值通过 |
| `metadata-values-rust-current.log` | 真实TCP/UDS自回环：ASCII规范化、多值顺序、含NUL/非UTF-8/逗号的两个`-bin`值，handler/Header/Trailer全通过 |
| `cli-candidate/init`、`cli-candidate/workspace-check-qualified.log` | 生成混合项目及整个 workspace all-targets 编译通过，使用显式本地候选覆盖 |
| `frozen-protobuf-compile-report.json` | 基线生成器在空输出目录重产的 36 个 Protobuf 文件与旧快照一致。原生成 client、server 源码和旧 builder 使用当前 runtime 编译通过 |
| `generated-thrift-feature-pair-report.json` | 同一份当前生成源码在独立 crate 中验证。关闭 thrift 时得到明确的 feature-gated 模块诊断，开启 thrift 后编译通过，两个 manifest 和 lock 分别留存 |

每组 basic/gzip/legacy 有11项行为、16次真实 RPC；raw 是构造 peer，不冒充 Kitex。keepalive 为真实 Go 策略的加速周期用例，生产默认分钟级周期未执行。裸 spawn 不自动继承 scope/取消，使用受管理任务的回收证据不能扩大到任意应用任务。

## 性能结果与未过门禁

性能保持固定数据集、同机同 lock/toolchain、每组至少三次；旧 Protobuf QPS≥0.95、p99≤1.10 的门槛没有放宽。Thrift 是新能力，不能与不同 payload 的 Protobuf 绝对吞吐比较。

- `perf-thrift-zlib-header-full/report.json`：当前 release 的完整矩阵 576/576 组通过，共 1728 个样本。覆盖三方向、四模式、TCP/UDS、三档并发、四种 payload 大小以及 identity/gzip。原默认配置下的两组高压 client-streaming 也通过，历史超时保留在各自报告中。
- `perf-thrift-boxed-full/report.json`：客户端transport装箱前的完整基线，三方向×四模式×TCP/UDS×1/32/256并发×64B/1KiB/64KiB/1MiB×identity/gzip，共576组、1728样本。574组通过；Rust自回环的TCP/UDS、client-streaming、256并发、1MiB、gzip 两组触发本地单RPC三秒时限，整体FAIL。错误样本均保留；未再出现原NO_ERROR/inactive-stream错误。
- `perf-thrift-eight-core`：同二进制、同三秒时限、从4个CPU增到8个CPU，两组仍FAIL。仅此证据不能把超时归因为CPU数量，也不能据此宣称协议死锁或资源泄漏。
- `perf-protobuf-controlled-final`：上一次完整24组配对结果19 PASS、5 FAIL；吞吐比约0.934～0.948，其中TCP/1并发/1MiB的p99比1.176。后续将managed生命周期分支单独装箱，服务future从4456降至3848字节（基线2880）；`perf-protobuf-lifecycle-box-executable` 三个小消息复测2 PASS，UDS/1/64B仍FAIL（QPS比0.931）。进一步将客户端受管理H2 future装箱，RPC future从4936降至2496字节（基线2384）。`perf-protobuf-transport-box-diagnostic`针对先前失败的5组复测：4组通过，TCP/1/1MiB仍FAIL（QPS比0.913、p99比1.060）。
- `codec-profile-current.json`：压缩优化前的 24 组分配探针，覆盖 4 种大小、identity/gzip 和 3 次重复。identity 的大 Bytes 共享输入切片，gzip 分配新输出并拼接输入。探针记录编码阶段的分配与存活字节。
- `perf-protobuf-delivery-full`：压缩和接收缓冲优化前的完整配对复测 21/24 达标。三项均为 TCP 单并发，64 B、1 KiB、1 MiB 的 p99 分别为基线的 1.103、1.159、1.141 倍，吞吐比为 0.961、0.987、0.951。
- `perf-protobuf-zlib-header-full/report.json`：当前 release 候选的完整配对复测 22/24 达标。TCP 单并发 64 B 和 UDS 单并发 64 KiB 的 p99 分别为基线的 1.158、1.162 倍，高于 1.10 门槛。两组吞吐比分别约为 0.95004 和 0.998。TCP 单并发 1 MiB 达标，保留全部三轮样本。
- `perf-thrift-delivery-high-default`：优化前的默认配置复测在 256 并发、1 MiB、gzip、client-streaming 两组触发三秒时限。显式设置服务端 `http2_max_concurrent_streams(64)` 的对照通过，TCP/UDS 的 p99 中位数分别为 1.78/1.85 秒。
- `profile-gates/gzip-current.report` 的 CPU 采样中，约 74% 的样本落在 miniz 压缩函数，约 11% 落在 memmove。逐段写入并取消输出过量预留后，两组仍超时，见 `perf-thrift-compression-segments-high`。再使用 `flate2` 1.1.11 的 zlib-rs 后端，默认配置两组通过，p99 中位数为 0.94/1.36 秒，见 `perf-thrift-zlib-high`。压缩级别、四个 CPU 和三秒门槛均与对照相同。
- `perf-protobuf-zlib-targeted`：TCP 64 B、TCP 1 KiB、UDS 1 MiB 三组达标，TCP 单并发 1 MiB 的 p99 为基线的 1.119 倍。相同基线二进制的 A/A 对照 p99 比 1.011，原始采样区间和二进制 hash 见 `perf-protobuf-1m-aa`。
- `perf-protobuf-zlib-header-1m` 使用最新二进制复测 TCP 单并发 1 MiB，3 轮的 p99 中位数比为 1.004，QPS 中位数比为 0.999。`perf-thrift-zlib-header-high` 的 TCP 和 UDS 高压组均通过，p99 中位数为 0.70 和 1.24 秒。完整复测使用相同二进制与门槛。
- `codec-profile-zlib-header.json` 的 24 组通过。100 条 1 MiB gzip 消息的分配请求字节中位数从 244.30 MB 降至 43.11 MB，额外存活字节峰值从 2.44 MB 降至 0.43 MB。已移除的输入拼接约为 104.86 MB，探针只计算编码路径的显式拼接和 allocator 数据。
- `perf-thrift-delivery-hooks-{off,on}`：同一二进制、TCP/UDS 各四个代表负载，每组 3 次、每次 3 秒。客户端 Noop hook 开启/关闭的 QPS 中位数比 0.903～1.060，p99 比 0.821～1.179。短 RPC 指标没有 Header/首消息/尾消息延迟的完整归因；这些数值不是 hook 本身的精确 CPU 或分配成本。
- `resource600-native-delivery/grpc-thrift-load/report.json`：当前原生 release 候选 TCP、16 并发、1 KiB、600 秒短 bidi，11,163,973 次调用、1,116,397 次预期 Drop、0 错误。预热基线→峰值→95 秒冷却后，客户端 FD 11→11→10、Tokio 活跃任务 1→34→0，服务端 FD 10→11→10。六项容量/回收断言均通过，原始 RSS 和时序保留。该用例测资源回收，handler/producer 的实际退出另见生命周期用例。

大消息接收按实际收到的 DATA 判断预留时机。收到跨过四分之一门槛的帧时先预留再追加，减少跨门槛时连续两次扩容。旧的渐进预留试验在 `perf-protobuf-adaptive-reserve` 中有原始结果。

## 已修正的问题与最简复现

| 问题、触发与原因 | 实际红/绿与复现 |
| --- | --- |
| 请求方向尚未半关闭，peer已发Header/Trailer后reset(NO_ERROR)；旧写错误覆盖真实终态 | `no-error-reset-flushed-before.log`、`queued-reset-before.log`已实际失败。后者peer发送4条空消息、flush Trailer并reset，消费者延迟5ms。当前 `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test peer_no_error_reset` 通过 |
| 将poll_reset的Reason转成通用h2 error会丢来源；合法write也可能返回inactive stream | 内部错误区分producer/reset/write/trailer/closed，仅上传关闭继续读真实响应；缺失终态、CANCEL、GOAWAY、producer错误仍走各自失败路径。`send-reset-origin-after.log` 26项定向回归；历史压力错误在 `perf-thrift-matrix`/`perf-thrift-rpc-diagnostics` 保留 |
| H2 slot被首流占150ms，第二请求1秒预算仍发完整1秒 | 已执行 `c3-slot-before.log` 红例和修复绿例；`cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test final_submission` |
| 普通 Protobuf 中间节点接收到 `grpc-timeout`，100 ms 后发起下游调用 | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --no-default-features --test final_submission protobuf_relay_inherits_the_received_deadline -- --exact`。期望第三节点看到一个扣除等待时间的头。修复前收到 0 个，见 `profile-gates/protobuf-relay-before.log`。原因是接收 deadline 只放进 Request 扩展。将同一绝对值放入请求上下文后，实测第三节点收到一个递减的预算，见 `profile-gates/protobuf-relay-pool-after.log` |
| 受管理 client 接收超过 16 KiB 的合法 `biz-extra` | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --no-default-features --test managed_pool oversized_business_metadata_is_rejected_at_the_h2_boundary -- --exact`。原路径使用 h2 的 16 MiB 默认值，实测接受大字段并返回 peer 原码。现在默认 16 KiB 时在 H2 层拒绝，显式配置 32 KiB 时完整收到 JSON 和 peer 原码。实际红绿见 `profile-gates/managed-header-cap-{before,after}.log` |
| UDS拨号地址被放进wire scheme，标准Go peer拒绝http+unix | `managed_pool::unix_dial_address_is_not_sent_as_an_http2_scheme` 通过，仍拨原UDS地址 |
| server-streaming误用unary提取器等待EOS；Go首请求即回复而Rust超时 | `server-open-before` 红/Go对照，`server-open-after`8项通过；首STOP请求且end_stream=false，额外DATA按Go忽略 |
| metainfo converter长key堆路径破坏prefix | 原Rust→Go FAIL在 `runner-lifecycle`；仅枚举当前transient/persistent安全导出后，三跳验证通过 |
| 默认 workspace 冷启动生成 Apache fixture 时，build script 从 Git 取 IDL | 用空 `OUT_DIR` 和 `GIT_ALLOW_PROTOCOL=file` 运行 `tests/code-generation` 的 build script，预期本地生成，实测退出 101。将固定上游的两个 IDL 和 NOTICE 放入 fixture 后，同一 builder 在空输出目录成功。与使用 Git 缓存的同版生成器比较，36 个 Protobuf 和 62 个 Thrift 输出逐字节相同。原始证据见 `cold-codegen-{before,after}.log`，根离线 check 见 `cold-codegen-candidate-check.log` |

Kitex handler直接返回ctx.Err()可在线上写13，专门raw fixture保留此行为；正例使用status.FromContextError，不把对端13改写为4。未知service原码20→Unknown(2)、method原码1→Cancelled(1)，原值可诊断，不据此自动重试。Go Legacy上游transient再传播只记迁移差异。

## 质量、依赖与交付

当前候选的根 check、无默认 feature 测试 139 项及文档测试 84 项、thrift/zstd 测试 155 项及文档测试 84 项通过。文档套件各另有 1 项原有 ignored。TLS/ShmIPC 组合编译通过。nightly fmt 和相关 clippy 完成，无新增诊断。zstd 检查比上一轮多启用了一处旧的 literal unwrap 告警，基线位置已与 HEAD 核对。原始输出见 `profile-gates/*zlib-header*.log` 和 `clippy-final-wire.log`。

新增生成服务用例的 nightly fmt 和 clippy 通过。clippy 使用 `cargo +nightly clippy --config target/thrift-grpc/candidate.toml --locked --offline -p thrift-grpc-codegen --test network`，只出现已有的 Pilota dead_code 和 nix future-incompat 诊断。原始输出见 `profile-gates/generated-network-{fmt,clippy}.log`。

早期无覆盖根 check 的来源 FAIL 保留在 `root-plain-check-current.{json,log}`。Pilota 新源码现已提交、推送到同名开发分支，Volo 的五个配套包整族锁到该真实 Git 来源；根 `cargo check --locked` 实际通过，无 path 配置、fixture 环境变量或临时依赖替换，见 `source-delivery-root-check.log`。`local-handoff/` 是源码交付前的历史快照，不描述当前远端状态。Volo 源码交付及下游依赖接入仍各自记录；用法见[分支交接](thrift-grpc-candidate.md)。

使用与配置见[thrift-grpc.md](thrift-grpc.md)，公开下游反馈见[D1](thrift-grpc-downstream-feedback.md)。真实发布、部署/CP/共用看板/迁移回滚、Mesh集成及所有ShmIPC运行/修复/高压按用户要求延期；远端 CI 恢复正常触发，历史未执行记录保留；ShmIPC仅编译，不计运行PASS。

## 新增边界的实际对照

- Go 通过 `WithGRPCUnknownServiceHandler` 启用兜底后，`/api.Missing/Bidi` 和 `/api.Interop/Missing` 均收到带方法名的 Header、回显 payload 和状态 0。`/api.Interop/Bidi` 继续由已注册 handler 处理。TCP/UDS 共 6 项通过。以 `grpc-thrift-boundaries --case-filter request/fallback-unknown-service,request/fallback-unknown-method,request/fallback-known-route` 定向运行。首次脚本误把响应的拼接 hex 串当片段列表，出现 6 项 oracle FAIL，见 `network-fallback`。修正比较并显式写入 payload 字段后，完整结果见 `network-routing-current`。
- 两方向真实Kitex legacy metadata.MD往返：每种传输分别验证ASCII、多值顺序和不透明`-bin`，含初始Header及成功Trailer。StreamX的公开Header/Trailer是`map[string]string`，源码默认无法返回多值；本用例明确用legacy API，不宣称该StreamX限制被消除。
- 最简畸形输入为 `/api.Interop/Bidi` 的 raw Binary payload `0b0001ffffffff00`，字段 1 长度为 −1，前置 5 字节 gRPC 头。`grpc-thrift-boundaries --case-filter request/binary-negative-length` 已实际执行，Rust 返回 13，Kitex 原码为 7，`grpc-message` 含 `invalid data length`。截断及未知 list/set/map 负长度同样分类。大容器和超深在 Rust 为 8，Go 使用 ProtocolError 的 type ID 7。早期按 13 预期 Go 的失败保留于 `network-metadata-boundary-split`，当前 276 项按实际码和错误文本通过。
- 关闭thrift的编译红例：`cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --no-default-features --test managed_pool`，原`serde_json::json!`在test日志中依赖可选feature，实测E0433（`grpc-protobuf-only-delivery-final.log`）。改用普通文本保留延迟数据后，全无默认feature套件通过（`grpc-protobuf-only-pool-final.log`），没有给Protobuf运行时新增JSON依赖。
- 同一无默认 feature 入口补测了无活跃调用的 PING 策略（`managed-pool-idle-keepalive.log`，7 项通过）：对端保持 socket 打开但不回 ACK，禁用 PING 或只对活跃流启用时复用连接；显式允许空闲 PING 时淘汰后重新拨号。真实 Kitex 的活跃空闲 bidi/违规 PING 仍以加速测试为证据。
- `network-biz-invalid-utf8` 单独补测了实际 HPACK 输入，`biz-extra` 的 JSON value 内含字节 `ff`。TCP/UDS 均保留原 `grpc-status: 13`，`BizStatus::from_status` 返回非法 JSON 错误。超大 header 另由 `managed_pool` 的原生 H2 peer 用例验证。
- `scripts/clippy-and-test.sh`接入无默认feature、thrift和生成fixture检查；脚本语法已验证；此前远端 CI 未执行，恢复后的结果以实际运行记录为准。源码/API/版本交接见[候选交接](thrift-grpc-candidate.md)。

## hook 成本与分阶段延迟

`hook-allocation/report.json` 为独立进程的实际 allocator 原型，9 组全部通过。未注册 hook 时，factory、安装、20 万次消息事件和终态回调的增量分配均为零；StreamContext 自身仍分配 4 次，不能把整个 RPC 说成零分配。单个 Noop 的 factory/安装各 1 次，双 Noop 组合各 3 次，消息与终态事件均不分配。原型在 dev profile 运行，其耗时不代替 release 吞吐结论。探针源码、运行命令及恢复原 checkout 的校验均留存。

可选 fixture 参数 `--phase-latency on` 从公开 hook 记录建流到 Header、首消息、尾消息与终态的 p50/p99，默认关闭，不改变 runtime。预热 32 次后验收 finish 数量与实际调用数一致、阶段顺序正确及回调齐全；最新 8192 个完成样本供统计。分阶段采样包含时钟和互斥锁开销，与未启用采样的完整性能矩阵分开报告。

`perf-protobuf-remaining-aa` 和 `perf-protobuf-remaining-ab` 对原全矩阵的两项 FAIL 做了同四核、同二进制、每轮五秒的三次配对诊断，两组均达原门槛。A/B 的 TCP 64 B 吞吐比 0.988、p99 比 0.943；UDS 64 KiB 吞吐比 1.002、p99 比 1.038。A/A 对照亦通过。此结果只说明该次对照没有复现退化，原完整矩阵 22/24 和全部 FAIL 样本保持原状。

`resource600-zlib-header/grpc-thrift-load/report.json` 对当前优化后二进制重复 600 秒资源验证，11,364,132 次调用、零错误，客户端 FD/任务和服务端 FD 的容量、95 秒冷却回收六项检查全部通过。

真实分支依赖接入后的新一轮检查见 `source-delivery-tests.json`：无默认 feature 与 thrift/zstd 套件各通过，分别保留84个通过、1个ignored的文档用例；volo-build/生成fixture通过；TTStream过滤回归44项通过。nightly fmt及相关clippy通过，clippy三处旧诊断分别为encode.rs两处literal unwrap和config_builder.rs的items-after-test-module，均在本轮改动前已存在。完整公共场景的可筛选输入和命令见[验证索引](thrift-grpc-verification.md)。

`phase-latency-representative/report.json` 的32组、96次实际release测量全部通过，覆盖四模式、TCP/UDS、identity/gzip、Rust→Rust及Rust→Go；每次finish数量与实际调用数一致，回调齐全且顺序正确。server-streaming每次回复3条消息，首尾延迟分别记录；unary/client-streaming以及单消息bidi的首尾可能相同。Go客户端未安装这套采样hook，其阶段延迟没有测量结论。

`network-cardinality-final/summary.json` 的292项全部通过，比上一轮增加10项unary零请求、HEADERS直接END_STREAM及单请求未半关闭的实际对照。raw边界现为212项，其余suite数量不变。原始Go HEADERS-only挂起见[验证索引](thrift-grpc-verification.md)，首轮错误oracle的两个FAIL保留，不把挂起算成功互通。

## G6 完成度复核补充

Go客户端新增 `phase-latency-go-representative/report.json`，16组、48次测量全部通过，四模式×TCP/UDS×identity/gzip。使用原H2 Header事件，三种流的消息事件来自公开StreamX回调，普通unary的唯一响应在生成client返回时记录；不把这个时间点解释成底层首次收到DATA。Go起点为应用调用，Rust起点为hook构造，跨语言阶段绝对值不能直接归因。首版自定义transport factory被Kitex gRPC初始化覆盖，导致采样缺失、业务RPC仍成功，原FAIL保留于 `go-phase-smoke`；改为公开事件后 `go-phase-smoke-v2`及完整16组通过，没有修改Kitex。

`codec-profile-completion.json` 的24组原始streaming与78组unary payload编码微基准通过：Args、success Result、声明异常Result和void，identity/gzip各3次。输入构造不计入编码分配，100条消息使用同一个encoder以单独观察包装成本，不能解释成100次实际unary RPC。大Bytes及异常FastStr的identity节点分别共享原输入；gzip分配输出且不共享该节点。void Result的编码长度始终为1字节STOP。

`queue-profile-completion.json`的6项通过。消息数上限1与编码字节上限2086分别实测，队列高水位为1条/1043字节或2条/2086字节；额外send实际Pending，消费者停顿至少5ms后恢复，完整排空且字节数一致。该测量只计队列中的编码数据，明确排除调用方未完成future和H2缓冲，不将主动停顿说成网络流控；真正零窗口等待由 `flow_control` 的H2 fixture验收。

`perf-protobuf-explicit-review.json` 对所有已记录、二进制/5秒时长/四核/阈值完全相同的A/B样本统一复核，没有丢弃原FAIL。24配置均达原0.95/1.10门槛，其中TCP64B、TCP1MiB、UDS64KiB各含每侧6次，其余每侧3次。原两项FAIL在全部6次上的吞吐比为0.960/1.000、p99比为1.039/1.094；与仅取后一次绿样本不同，原完整矩阵和后续样本全部计入。该报告是计划允许的显式复核，原完整矩阵仍保留22/24结果，不能改名为一次新的全矩阵PASS。

计划中的虚构目标 `grpc_plan_regressions` 已替换为实际四个公共测试目标，并在关闭Thrift时执行，见 `public-prerequisite-command-audit.log`。fixture的nightly严格clippy通过，公共库的原有三处诊断仍单列，不冒称整个workspace的 `-D warnings` 已通过。

Go采样器的 `go test -race ./oracle` 通过；另以race构建的真实Go客户端执行四模式、32并发、每组三次TCP smoke，`go-phase-race-network/report.json` 四组通过且没有race诊断。该跑法仅验证采样器并发正确性，不计入release性能结论。
