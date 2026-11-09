# Thrift gRPC 开发分支交接

沿用 `feat/ttheader-streaming-20261105`；按 Pilota→Volo→下游的顺序交付现有开发分支。源码提交、推送与 crate/tag 发布分开记录；本轮不执行正式发布、主干合并或部署；2026-11-11 起恢复远端 CI。完整结果和未过门禁见[实施状态](thrift-streaming-over-grpc-status.md)。

## 版本与兼容边界

| 包 | 当前源码版本 | 后续候选方案，未应用/发布 |
| --- | --- | --- |
| volo-build | 0.13.0-alpha.1 | 0.13.0-alpha.2，新增显式thrift_grpc入口；旧thrift/protobuf入口保留 |
| volo-grpc | 0.12.2 | 源码兼容扩展拟用0.12.3-alpha.1；如果进一步审查发现公共破坏，应改0.13.0候选并记录迁移 |
| volo-cli | 0.12.1 | 与新build入口配套候选；不单独承诺旧生成器能生成新入口 |
| volo/volo-thrift | 现有版本 | 无需为payload引入新协议依赖；公共网络API只在实际涉及处变化 |

这里的候选号段仅为交接方案。Rust的prerelease版本匹配须在真实选定号段后更新所有相关version约束及锁；本轮依赖仍消费具名分支源码，不声称registry已有这些API。

| 源码组合 | 结果或支持边界 |
| --- | --- |
| 既有Protobuf生成/手写EntryMessage + 当前runtime，thrift关闭 | 默认类型参数/旧构造器/alias/newtype/自定义Decoder及默认codec编译回归；C1～C4公共行为修复同时生效 |
| 当前新Thrift gRPC生成 + 当前runtime/thrift | 四模式/混合service、unary Args/Result/throws/void，共同候选已实际双向互通 |
| 当前新Thrift gRPC生成 + thrift关闭或旧runtime | 不支持，明确缺少模块/feature；不静默退回TTHeader或Protobuf |
| 旧Thrift/TTStream生成入口 | 语义不变；39项TTStream回归与共享消息类型fixture独立验证 |
| 同一IDL双传输 | 共享Pilota数据和方法级异常enum，外部adapter只重导出；不靠转换或重编码制造相同类型 |

C1坏details、C2建流即过期、C3出站剩余预算、C4有限排空是公共修复review单元，不藏在thrift feature后。取消不再依赖应用继续recv；裸spawn的应用责任和受管理spawn的回收契约见使用文档。默认30秒排空改变原无限等待行为，应用需要更长排空时显式配置；已提交请求不自动重放。

## 本地复现与来源

`target/thrift-grpc/local-handoff/`保存源码交付前的补丁、锁和验证快照。manifest 中的基线 commit、远端 head、工作树 hash 和实际依赖只描述该次快照，不代表之后的分支状态。早期普通 cargo check 因远端 Pilota 尚无新 API 而失败，原始输出保留。

Pilota 的当前源码已交付同名开发分支，五个配套包锁定同一真实 Git 来源。Volo 根目录的 `cargo check --locked` 已实际通过，无 path 覆盖或 fixture 环境变量，见 `target/thrift-grpc/source-delivery-root-check.log`。Volo 交付后，下游再整族更新锁并重复普通根 check；Cargo.toml 使用 branch，Cargo.lock 记录解析 SHA，不能把 context/runtime 拆成 Git 与 registry 两份。

公共复现/反馈见[D1](thrift-grpc-downstream-feedback.md)；所有性能 FAIL 与未执行项保持。开发分支提交和推送不再使用 CI 跳过标记。现有 GitHub 工作流由 pull_request 或 main 分支 push 触发；功能分支推送本身不触发该工作流，结果以实际运行记录为准。不发布或部署，不引入部署专用信息。
