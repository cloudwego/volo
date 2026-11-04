# 共享 Rust fixture 输入

`../fixtures/interop.thrift` 是 Interop schema 唯一真源，保持 Go 默认 requiredness。`src/lib.rs` 只转换 namespace 和 Message 两个字段，追加独立的 `../fixtures/rust-pure-extension.thrift`。结构变换必须各命中一次，漂移会明确报错。

继承直接读取 `../go/idl/audit.thrift` 与 `audit_parent.thrift`，不维护 Rust 镜像。Volo 的 build.rs 通过仓内相对路径包含这份 Rust helper；其他测试消费者也可以包含同一份源码，无需发布新的 build dependency。Rust view 写入 `$OUT_DIR/shared-fixtures/interop.thrift`；源文件和工具目录均应声明为 Cargo 重建输入。

本包只用于测试支持（`publish=false`、无依赖）。在共享工具根目录下可以独立运行：

```sh
cargo +nightly test --manifest-path rust-fixtures/Cargo.toml
cargo +nightly fmt --manifest-path rust-fixtures/Cargo.toml --all -- --check
cargo +nightly clippy --manifest-path rust-fixtures/Cargo.toml --all-targets -- -D warnings
```

修改 Message 或 namespace 时，应同步审阅适配规则并重跑 Go 生成与 Rust consumers。不要编辑 OUT_DIR 文件或提交另一份 Rust IDL 镜像。
