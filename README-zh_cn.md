![Volo](https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true)

[![Crates.io](https://img.shields.io/crates/v/volo)](https://crates.io/crates/volo)
[![Documentation](https://docs.rs/volo/badge.svg)](https://docs.rs/volo)
[![Website](https://img.shields.io/website?up_message=cloudwego&url=https%3A%2F%2Fwww.cloudwego.io%2F)](https://www.cloudwego.io/)
[![License](https://img.shields.io/crates/l/volo)](#license)
[![Build Status][actions-badge]][actions-url]

[actions-badge]: https://github.com/cloudwego/volo/actions/workflows/ci.yaml/badge.svg
[actions-url]: https://github.com/cloudwego/volo/actions

[English](README.md) | 中文 | [日本語](README-ja.md)

Volo 是字节跳动服务框架团队研发的 **高性能**、**可扩展性强** 的 Rust RPC 框架，使用了 Rust 最新的 AFIT 和 RPITIT 特性。

Volo 使用 [`Motore`][motore] 作为其中间件抽象层, Motore 基于 AFIT 和 RPITIT 设计。

## 概览

### Crates

Volo 主要包含 6 个 crate 库:

1. [`volo`][volo] - 包含框架的通用组件。
2. [`volo-thrift`][volo-thrift] - 提供 **thrift** RPC 消息协议支持。
3. [`volo-grpc`][volo-grpc] - 提供 **gRPC** RPC 消息协议支持。
4. [`volo-build`][volo-build] - 通过 **thrift** 或 **protobuf** 文件生成 rust 代码。
5. [`volo-cli`][volo-cli] - 命令行工具，基于 thrift 和 protobuf 的 IDL 生成 项目脚手架。
6. [`volo-macros`][volo-macros] - 框架的中间件抽象层。

### 特点

#### 使用 AFIT 和 RPITIT 特性

Volo 使用 [`Motore`][motore] 作为其中间件抽象层, Motore 基于 AFIT 和 RPITIT 设计。

通过 RPITIT，我们可以避免很多不必要的 Box 内存分配，以及提升易用性，给用户提供更友好的编程接口和更符合人体工程学的编程范式。

#### 高性能

Rust 以高性能和安全著称，我们在设计和实现过程中也时刻以高性能作为我们的目标，尽可能降低每一处的开销，提升每一处实现的性能。

首先要说明，**和 Go 的框架对比性能是极不公平的**，因此我们不会着重比较 Volo 和 Kitex 的性能，并且我们给出的数据仅能作为参考，希望大家能够客观看待；同时，由于在开源社区并没有找到另一款成熟的 Rust 语言的 Async 版本 Thrift RPC 框架，而且性能对比总是容易引战，因此我们希望尽可能弱化性能数据的对比，仅会公布我们自己极限 QPS 的数据。

在和 Kitex 相同的测试条件（限制 4C）下，Volo 极限 QPS 为 35W；同时，我们内部正在验证基于 [Monoio](https://github.com/bytedance/monoio)（CloudWeGo 开源的 Rust Async Runtime）的版本，极限 QPS 可以达到 44W。

从我们线上业务的火焰图来看，得益于 Rust 的静态分发和优秀的编译优化，框架部分的开销基本可以忽略不计（不包含 syscall 开销）。

#### 易用性好

~~Rust 以难学难用而闻名~~，我们希望尽可能降低用户使用 Volo 框架以及使用 Rust 语言编写微服务的难度，提供最符合人体工程学和直觉的编码体验。因此，我们把易用性作为我们最重要的目标之一。

比如，我们提供了 volo 命令行工具，用于初始化项目以及管理 idl；同时，我们将 thrift 及 gRPC 拆分为两个独立（但共用一些组件）的框架，以提供最符合不同协议语义的编程范式及接口。

我们还提供了`#[service]`宏（可以理解为不需要 `Box` 的 `async_trait`）来使得用户可以无心理负担地使用异步来编写 `Service` 中间件。

#### 扩展性强

收益于 Rust 强大的表达和抽象能力，通过灵活的中间件 Service 抽象，开发者可以以非常统一的形式，对 RPC 元信息、请求和响应做处理。

比如，服务发现、负载均衡等服务治理功能，都可以以 Service 形式进行实现，而不需要独立实现 Trait。

相关的扩展，我们会放在 [volo-rs](https://github.com/volo-rs) 组织下，也欢迎大家贡献自己的扩展到 volo-rs。

查看 [guide](https://www.cloudwego.io/zh/docs/volo/guide/) 获取更多信息。

## 相关教程

Volo-Thrift: https://www.cloudwego.io/zh/docs/volo/volo-thrift/getting-started/

Volo-gRPC: https://www.cloudwego.io/zh/docs/volo/volo-grpc/getting-started/

## 示例

参考[Examples](examples).

## 相关生态

- [Volo-rs][volo-rs]: Volo 的相关生态，包含了 Volo 的许多组件
- [Motore][motore]: Volo 参考 Tower 设计的，使用了 AFIT 和 RPITIT 的 middleware 抽象层
- [Pilota][pilota]: Volo 使用的 Thrift 与 Protobuf 编译器及编解码的纯 Rust 实现（不依赖 protoc）
- [Metainfo][metainfo]: Volo 用于进行元信息透传的组件，期望定义一套元信息透传的标准

## 开发路线图

点击 [ROADMAP.md](https://github.com/cloudwego/volo/blob/main/ROADMAP.md) 获取更多信息。

## 如何贡献

点击 [CONTRIBUTING.md](https://github.com/cloudwego/volo/blob/main/CONTRIBUTING.md) 获取更多信息。

## 开源许可

Volo 使用 MIT license 和 the Apache License (Version 2.0) 双重许可证。

点击 [LICENSE-MIT](https://github.com/cloudwego/volo/blob/main/LICENSE-MIT) 和 [LICENSE-APACHE](https://github.com/cloudwego/volo/blob/main/LICENSE-APACHE) 查看更多细节。

## 鸣谢

我们使用了一些第三方组件, 在此感谢他们的付出

点击 [CREDITS.md](https://github.com/cloudwego/volo/blob/main/CREDITS.md) 查看完整的名单。

## 社区

- Email: [volo@cloudwego.io](mailto:volo@cloudwego.io)
- 如何成为 member: [COMMUNITY MEMBERSHIP](https://github.com/cloudwego/community/blob/main/COMMUNITY_MEMBERSHIP.md)
- Issues: [Issues](https://github.com/cloudwego/volo/issues)
- 飞书用户群: 通过 [Feishu](https://www.feishu.cn/) app 扫描下方的二维码 或者 [点击连接](https://applink.feishu.cn/client/chat/chatter/add_by_link?link_token=b34v5470-8e4d-4c7d-bf50-8b2917af026b) 加入我们的 CloudWeGo Volo 用户群。

  <img src="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-feishu-user-group.png" alt="Volo user group" width="50%" height="50%" />

[volo-rs]: https://github.com/volo-rs
[motore]: https://github.com/cloudwego/motore
[pilota]: https://github.com/cloudwego/pilota
[metainfo]: https://github.com/cloudwego/metainfo
[volo]: https://docs.rs/volo
[volo-thrift]: https://docs.rs/volo-thrift
[volo-grpc]: https://docs.rs/volo-grpc
[volo-build]: https://docs.rs/volo-build
[volo-cli]: https://crates.io/crates/volo-cli
[volo-macros]: https://docs.rs/volo-macros
[examples]: https://github.com/cloudwego/volo/tree/main/examples
