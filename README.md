![Volo](https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true)

[![Crates.io](https://img.shields.io/crates/v/volo)](https://crates.io/crates/volo)
[![Documentation](https://docs.rs/volo/badge.svg)](https://docs.rs/volo)
[![Website](https://img.shields.io/website?up_message=cloudwego&url=https%3A%2F%2Fwww.cloudwego.io%2F)](https://www.cloudwego.io/)
[![License](https://img.shields.io/crates/l/volo)](#license)
[![Build Status][actions-badge]][actions-url]

[actions-badge]: https://github.com/cloudwego/volo/actions/workflows/ci.yaml/badge.svg
[actions-url]: https://github.com/cloudwego/volo/actions

English | [中文](README-zh_cn.md)

Volo is a **high-performance** and **strong-extensibility** Rust RPC framework that helps developers build microservices.

Volo uses [`Motore`][motore] as its middleware abstraction, which is powered by GAT.

## Overview

### Crates

Volo mainly consists of six crates:

1. The [`volo`][volo] crate, which contains the common components of the framework.
2. The [`volo-thrift`][volo-thrift] crate, which provides the Thrift RPC implementation.
3. The [`volo-grpc`][volo-grpc] crate, which provides the gRPC implementation.
4. The [`volo-build`][volo-build] crate, which generates thrift and protobuf code.
5. The [`volo-cli`][volo-cli] crate, which provides the CLI interface to bootstrap a new project and manages the idl files.
6. The [`volo-macros`][volo-macros] crate, which provides the macros for the framework.

### Features

#### Powered by GAT

Volo uses [`Motore`][motore] as its middleware abstraction, which is powered by GAT.

Through GAT, we can avoid many unnecessary `Box` memory allocations, improve ease of use, and provide users with a more friendly programming interface and a more ergonomic programming paradigm.

#### High Performance

Rust is known for its high performance and safety. We always take high performance as our goal in the design and implementation process, reduce the overhead of each place as much as possible, and improve the performance of each implementation.

First of all, it is very unfair to compare the performance with the Go framework, so we will not focus on comparing the performance of Volo and Kitex, and the data we give can only be used as a reference, I hope everyone can view it objectively; at the same time, due to the open source community has not found another mature Rust async version Thrift RPC framework, and performance comparison is always easy to lead to war, so we hope to weaken the comparison of performance data as much as possible, and we'll only publish our own QPS data.

Under the same test conditions as Kitex (limited to 4C), the Volo QPS is 350k; at the same time, we are internally verifying the version based on Monoio (CloudWeGo's open source Rust async runtime), and the QPS can reach 440k.

From the flame graph of our online business, thanks to Rust's static distribution and excellent compilation optimization, the overhead of the framework part is basically negligible (excluding syscall overhead).

#### Easy to Use

~~Rust is known for being hard to learn and hard to use~~, and we want to make it as easy as possible for users to use the Volo framework and write microservices in the Rust language, providing the most ergonomic and intuitive coding experience possible. Therefore, we make ease of use one of our most important goals.

For example, we provide the volo command line tool for bootstraping projects and managing idl files; at the same time, we split thrift and gRPC into two independent(but share some components) frameworks to provide programming paradigms that best conform to different protocol semantics and interface.

We also provide the `#[service]` macro (which can be understood as the `async_trait` that does not require `Box`) to enable users to write service middleware using async rust without psychological burden.

#### Strong Extensibility

Benefiting from Rust's powerful expression and abstraction capabilities, through the flexible middleware `Service` abstraction, developers can process RPC meta-information, requests and responses in a very unified form.

For example, service governance functions such as service discovery and load balancing can be implemented in the form of services without the need to implement Trait independently.

We have also created an organization [`Volo-rs`][volo-rs], any contributions are welcome.

For more information, you may refer to [our guide](https://www.cloudwego.io/zh/docs/volo/guide/).

## Tutorial

Volo-Thrift: https://www.cloudwego.io/zh/docs/volo/volo-thrift/getting-started/

Volo-gRPC: https://www.cloudwego.io/zh/docs/volo/volo-grpc/getting-started/

## Examples

See [Examples][examples].

## Related Projects

- [Volo-rs][volo-rs]: The volo ecosystem which contains a lot of useful components.
- [Motore][motore]: Middleware abstraction layer powered by GAT.
- [Pilota][pilota]: A thrift and protobuf implementation in pure rust with high performance and extensibility.
- [Metainfo][metainfo]: Transmissing metainfo across components.

## RoadMap

See [ROADMAP.md](https://github.com/cloudwego/volo/blob/main/ROADMAP.md) for more information.

## Contributing

See [CONTRIBUTING.md](https://github.com/cloudwego/volo/blob/main/CONTRIBUTING.md) for more information.

## License

Volo is dual-licensed under the MIT license and the Apache License (Version 2.0).

See [LICENSE-MIT](https://github.com/cloudwego/volo/blob/main/LICENSE-MIT) and [LICENSE-APACHE](https://github.com/cloudwego/volo/blob/main/LICENSE-APACHE) for details.

## Credits

We have used some third party components, and we thank them for their work.

For the full list, you may refer to the [CREDITS.md](https://github.com/cloudwego/volo/blob/main/CREDITS.md) file.

## Community

- Email: [volo@cloudwego.io](mailto:volo@cloudwego.io)
- How to become a member: [COMMUNITY MEMBERSHIP](https://github.com/cloudwego/community/blob/main/COMMUNITY_MEMBERSHIP.md)
- Issues: [Issues](https://github.com/cloudwego/volo/issues)
- Feishu: Scan the QR code below with [Feishu](https://www.feishu.cn/) or [click this link](https://applink.feishu.cn/client/chat/chatter/add_by_link?link_token=b34v5470-8e4d-4c7d-bf50-8b2917af026b) to join our CloudWeGo Volo user group.

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
