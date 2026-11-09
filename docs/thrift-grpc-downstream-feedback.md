# Thrift gRPC 下游候选反馈 D1

2026-11-11。本材料只描述公共扩展点、可公开复现的问题和结果；不包含下游仓库、配置、目标名或原始运行日志。开发分支按上游依赖顺序交付；每次验证以实际锁、源码和二进制 hash 为准，不能只凭分支名判断。

## 接线结果

下游通过现有 motore 建流层、`StreamScope`、builder hook factory 和 `StreamHook` 接入上下文、配额与观测。Header 返回后仍接收消息/终态事件；回收与 span 结束由全流 owner 负责。无需包装每一层 Sender/RecvStream 泛型，也不需要将部署字段硬编码到公共 runtime。

固定 metainfo 契约为 transient 一跳、persistent 多跳；不要求实现 Kitex 的 bugfix 兼容 flag。初始 Header 和最终 Trailer 分开导入，非空终态值覆盖初始值，空/缺失字段保留已知值。下游位置字段处理通过 hook 完成，公共层只负责标准 metainfo 与终态生命周期。

## 已反馈并修正的公共问题

| 问题及触发条件 | 原结果、预期与原因 | 公共复现及实际状态 |
| --- | --- | --- |
| UDS 地址作为 HTTP/2 scheme | 原请求携带 `http+unix`，标准 Go peer 拒绝；预期拨号使用 UDS，wire scheme 仍为 http | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test managed_pool unix_dial_address_is_not_sent_as_an_http2_scheme`；已执行，通过 |
| 终态后 NO_ERROR reset 尚未关闭的上传方向 | 原 Internal 覆盖已发出的真实终态；预期保留终态。h2 reset 可先于响应 future 消费，必须继续读取真实 Header/Trailer | `cargo --config target/thrift-grpc/candidate.toml test -p volo-grpc --test peer_no_error_reset`；修复前稳定失败，修复后通过；缺失 status/CANCEL 仍失败 |
| server-streaming 首包后请求方向保持打开 | 复用 unary 提取器会等 EOS，Go 已可返回响应而 Rust 超时；预期首请求即可进入 handler | 共用 runner 的 `grpc-thrift-boundaries`；已执行，两方向 TCP/UDS 通过；额外请求按固定 Go 行为忽略 |
| 大消息零散到达时反复扩容 | 不做任何预留会重复复制；直接按 peer 长度头预留又有资源风险 | 收到至少四分之一 payload 后再预留剩余空间；codec/恶意输入回归通过，完整性能复核仍独立记录，不把针对性改善当全部性能通过 |

`network-routing-current/summary.json` 的公开 profile 共 282 项通过，覆盖基本/gzip/legacy、202 个 raw 边界、4 个 metadata 多值和二进制组合、28 个生命周期、32 个三跳及 4 个 keepalive 组合。下游候选复用公共 runner，结果单独记录。

## 剩余交接边界

性能、资源、依赖来源和版本方案以各自真实报告为准；历史 FAIL 保留。原生候选独立运行的 600 秒资源回收已通过，另有下游接入的独立报告。尚未交付的源码不能描述为远端 fresh checkout 已验证。

本轮在现有开发分支交付源码，不进行正式发布、主干合并或部署。2026-11-11 起恢复远端 CI，历史未执行记录保留。服务网格和 ShmIPC 运行场景按约定延期；未执行事项不计 PASS。

补充基数审计后的 `network-cardinality-final` 共292项通过，raw边界212项。HEADERS直接END_STREAM的空unary在Rust及时返回3、Go在8秒raw读时限内未回复；该Go差异保留原始失败，只要求Rust有界拒绝，不复刻挂起。
