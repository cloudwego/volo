# Thrift gRPC 验证与复现索引

本索引把计划 I01～I32 的公共场景对应到实际 fixture。I20 是下游专属扩展，不在公共运行时中实现。每次运行的版本、命令、输入、预期码、实际码和二进制 hash 由 JSON 报告保存；下表不把源码推断当成运行结果。

## 共用入口

在仓库根执行，Python 环境需安装 runner 的 `pyproject.toml` 中声明的依赖：

```bash
cargo build --release --locked -p thrift-grpc-codegen
go -C tests/interop/kitex-streaming/go build -mod=readonly -buildvcs=false \
  -o "$PWD/target/grpc-oracle" ./cmd/oracle
PYTHONPATH=tests/interop/kitex-streaming/python python3 -m kitex_streaming_interop \
  --profile upstream --protocol grpc-thrift \
  --suite grpc-thrift-basic,grpc-thrift-gzip,grpc-thrift-legacy,grpc-thrift-metadata-values,grpc-thrift-boundaries,grpc-thrift-lifecycle,grpc-thrift-multihop,grpc-thrift-keepalive \
  --go-bin target/grpc-oracle --rust-bin target/release/thrift-grpc-codegen \
  --transport tcp,uds --direction both --out target/thrift-grpc/reproduced
```

网络 IDL 位于 `tests/interop/kitex-streaming/go/idl/grpc_{thrift,parent,no_namespace}.thrift`；生成类型、共享数据和 Rust 重命名 fixture 另见 `tests/thrift-grpc-codegen`。`basic` 每个方向/传输组合实际调用普通 unary、显式 unary、client/server/bidi 五类方法；4 个运行组覆盖计划的 20 个基础组合。raw suite 使用构造的 HTTP/2 peer，明确区别于真实 Kitex 生成客户端。

只复现一个边界时，将上述 suite 换成 `grpc-thrift-boundaries` 并传 `--case-filter request/<name>` 或 `response/<name>`；可用逗号选择多个。以下边界名称均来自实际报告。无 Thrift 公共修复可用 `cargo test --locked -p volo-grpc --no-default-features --test <target>` 独立运行；Thrift codec 用例需增加 `--features thrift`。

| ID | 具体输入或 fixture | 预期与实际验证入口 |
| --- | --- | --- |
| I01 | `grpc-thrift-basic`、`grpc-thrift-gzip`、`grpc-thrift-legacy`；流消息数 0/1/N | 每条回复及成功 Trailer 均比较；StreamX 与 legacy API 分开验收 |
| I02 | `request/prefix-every-byte,request/coalesced-messages`；`response/prefix-split-every-byte,response/coalesced-responses` | 分片/合帧保序；`codec_compat` 同时验证 payload 不是 Thrift Message envelope |
| I03 | `request/raw-bidi-empty-stop,request/zero-byte-thrift-payload,request/unknown-nesting-within-budget,request/unknown-nesting-over-budget` | `[00]` 双向成功；空字节独立失败，容器/递归预算有界拒绝；Pilota golden 补未知字段和演进 |
| I04 | `volo-build/tests/thrift_grpc.rs`、`grpc_parent.thrift`、`grpc_no_namespace.thrift` | 原 Go ServiceInfo path、include/继承、无 namespace、Rust rename 与实际路由逐项比较 |
| I05 | `request/unknown-service,request/unknown-method,request/fallback-unknown-service,request/fallback-unknown-method,request/fallback-known-route` | 默认 Go 原码20/1、Rust12分别固定；显式 Go fallback 回显成功；重复注册由 `network` 拒绝 |
| I06 | `grpc-thrift-lifecycle`、`application_flow` | delayed Header 不阻塞建流；4096条消息/1KiB窗口复现串行收发停滞，取消并改并发驱动后完成 |
| I07 | `request/client-empty-stream,request/server-open-request,request/server-two-requests` | 正常 EOF 不取消；server 首请求即可回复，后续请求按 Go 容错忽略 |
| I08 | `response/unary-missing-response,response/unary-extra-response,response/client-extra-response,response/error-after-unary-value` | 唯一响应检查；首值不能覆盖最终错误 |
| I09 | `request/headers-only-empty-stream,response/missing-terminal-status,response/truncated-body,response/truncated-prefix` | trailer-only 保留状态，缺少成功终态不返回成功 EOF；`peer_no_error_reset` 补实际 reset 竞态 |
| I10 | `biz-status-unicode-json`、`response/biz-unicode-json`、`response/biz-invalid-json,response/biz-invalid-utf8` | 中文、引号、反斜杠和转义控制字符保真；非法 JSON/UTF-8 分类拒绝，保留 peer 原码 |
| I11 | `grpc-thrift-metadata-values`、`metadata_values`、`streaming::metadata::tests` | 多值顺序、含 NUL/非 UTF-8 的 binary 值保真；Header/Trailer backward 分阶段覆盖 |
| I12 | `grpc-thrift-multihop`、`long_forward_keys_keep_their_prefix_and_upstream_is_single_hop` | 32项三跳实测；rpc-transit 一跳、rpc-persist 多跳，不增加兼容 flag；本跳显式新值可以发送 |
| I13 | `request/zero-budget`、`final_submission`、`grpc-thrift-lifecycle`/`multihop` deadline 场景 | handler=0 的已过期请求、扣除 layer/握手/slot 等待、唯一 timeout 头、Header 后超时及级联退出 |
| I14 | `grpc-thrift-lifecycle` 的 cancel/disconnect；`public_cancellation` | handler 不读消息、请求已半关闭、客户端退出时，实际 handler/受管理下游 Drop，不只观察 token |
| I15 | `peer_no_error_reset`、`flow_control`、`public_cancellation` | Header/终态竞态、response Drop、正常 EOF；只发布一次终态，不影响其他 RPC |
| I16 | `flow_control`、`streaming` 模块测试 | producer pending/error/panic 和受管理任务实际回收；裸 spawn 的显式上下文/取消责任见使用文档 |
| I17 | `managed_pool::short_rpc_latency_with_partially_and_fully_occupied_long_stream_pool` | TCP/UDS各3次、长流未满/占满时各64次短 RPC；真实连接计数、等待、p99、取消复用和 idle 回收 |
| I18 | `grpc-thrift-gzip`；`request/invalid-frame-flag,response/unknown-encoding,response/gzip-corrupt,response/gzip-decompression-budget` | identity/gzip 分开；未知编码、坏 flag/压缩和解压预算有界失败 |
| I19 | `public_cancellation`、`managed_pool::goaway_retires_admission_without_replaying_or_aborting_an_existing_stream`、lifecycle drain | 新调用拒绝，已有流限期排空，pending handler/producer 到期回收，run future 返回，不重放 |
| I21 | `codec_compat`、`frozen-protobuf-compile-report.json`、TTStream测试、feature-pair报告 | 旧生成 client/server 对当前 runtime 编译；旧构造器保留，新生成源码缺 feature 清晰失败 |
| I22 | `volo-build/tests/thrift_grpc.rs`、Pilota IDL matrix | 所有 service 完整校验；显式 unary 要求单参数，无注解允许零/多参数；streaming throws/oneway 拒绝 |
| I23 | basic 的 `unary-args-result`、`zero-void-stop`、`multiple-parameters-field-ids`、`container-result`；payload golden | 参数字段 ID、scalar/container/typedef、Args/Result 包装和 void STOP 与 Go 一致 |
| I24 | basic 的 `multiple-declared-exceptions`、`single-declared-void-exception`；`network` 共享 enum；`response/error-after-unary-value` 和 Pilota Result golden | 每个声明 variant 都实际返回并匹配；单异常同为方法级 enum，错误 Trailer 优先；增加 variant 的穷尽 match 变化单列 |
| I25 | `network::unary_progresses_on_the_same_connection_as_three_active_streams` | 实际 accept=1；三类流活跃、server-stream受流控期间 unary 成功，取消该流后另两流仍成功 |
| I26 | `request/unary-zero-requests,request/unary-header-end-stream,request/unary-two-requests,request/unary-open-request`；lifecycle半关闭取消 | Rust严格检查零/多请求及未END_STREAM；Go首请求即调用的差异保留，不伪造双方相同行为 |
| I27 | `codec_compat`；旧/新生成独立编译 fixture | turbofish、alias/newtype、Unpin/Send、自定义 Decoder/有状态 Encoder、identity/gzip 全路径实际可替换 |
| I28 | `grpc-thrift-keepalive`、`managed_pool` keepalive 三项测试 | 加速周期的 idle bidi 存活并恢复收发；无活跃流 opt-in，违规 ping 原 Go 报 too_many_pings，未ACK回收。生产分钟级默认未运行 |
| I29 | `thrift_content_type`；`request/wrong-subtype,request/non-grpc-content-type,request/missing-content-type` | 路由后 decode 前拒绝；HTTP/2 subtype错误12，显式HTTP/1非gRPC返回HTTP415，handler=0；Proto保持基线 |
| I30 | `response/bad-base64-header,response/bad-base64-final-trailer,response/details-padded,response/details-unpadded`；`codec_compat` | 坏base64返回错误不panic、不成功EOF；合法details原样保留，公共C1不依赖Thrift |
| I31 | `network`、`flow_control`、`hook-allocation/report.json` | call返回后hook仍有效；终态一次；9组实际分配原型区分context、hook安装、消息与终态成本 |
| I32 | `network::dual_transport_modules_share_messages_and_exception_enum_identity`、`thrift_content_type` 的多服务测试 | TTStream/gRPC直接传递同一消息/异常；Proto与Thrift不同NamedService共存，不提供方法级payload混编 |

## 失败记录与性能边界

[实施状态](thrift-streaming-over-grpc-status.md)保留修复前实际红例、未执行的红例以及修复后的绿例。R1～R4的最小输入、原因、预期/实际和命令在[计划第10节](thrift-streaming-over-grpc-plan.md)；旧树未实际运行的用例明确标为源码证明。不要将修复后的通过报告覆盖历史失败日志。

完整性能报告、后续 A/A/A/B 诊断和资源报告独立保存。`tests/perf/thrift-grpc.py --phase-latency on` 为 Rust 客户端记录 Header/首消息/尾消息/终态，包含采样开销；不替代默认关闭采样的吞吐矩阵，也Go客户端由 `grpc_phase_latency.go` 复用Header/StreamX事件，普通unary消息时间在生成client返回时记录；对应16组48次单独报告。各方起点/观测点和采样开销都显式标注。identity 与 gzip 的复制/分配、Noop hook 的注册/调用成本、600秒回收均单独验收。

补充 unary 基数审计已实际运行：零 DATA 后发送 END_STREAM 时 Rust 返回3、Go返回13；HEADERS直接END_STREAM时Rust返回3，而Go在raw peer的8秒读时限内无回复；单请求但不半关闭、预算150ms时Rust返回4，Go直接成功。`unary-cardinality-audit` 保留最初按Go返回13预期而失败的HEADERS-only两项。后续正式用例将该输入作为Rust及时拒绝的增强验收，Go挂起记录为差异，不要求Rust复刻。
