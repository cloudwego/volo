<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-light.png?sanitize=true" />
  <source media="(prefers-color-scheme: dark)" srcset="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-dark.png?sanitize=true" />
  <img alt="Volo" src="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-light.png?sanitize=true" />
</picture>

[![Crates.io](https://img.shields.io/crates/v/volo)](https://crates.io/crates/volo)
[![Documentation](https://docs.rs/volo/badge.svg)](https://docs.rs/volo)
[![Website](https://img.shields.io/website?up_message=cloudwego&url=https%3A%2F%2Fwww.cloudwego.io%2F)](https://www.cloudwego.io/)
[![License](https://img.shields.io/crates/l/volo)](#license)
[![Build Status][actions-badge]][actions-url]

[actions-badge]: https://github.com/cloudwego/volo/actions/workflows/ci.yaml/badge.svg
[actions-url]: https://github.com/cloudwego/volo/actions

[English](README.md) | [中文](README-zh_cn.md) | 日本語

Voloは、開発者がマイクロサービスを構築するのを支援する**高性能**で**強力な拡張性**を持つRust RPCフレームワークです。

Voloは、AFITとRPITITによって強化されたミドルウェア抽象として[`Motore`][motore]を使用します。

## 概要

### クレート

Voloは主に6つのクレートで構成されています：

1. フレームワークの共通コンポーネントを含む[`volo`][volo]クレート。
2. Thrift RPC実装を提供する[`volo-thrift`][volo-thrift]クレート。
3. gRPC実装を提供する[`volo-grpc`][volo-grpc]クレート。
4. HTTP実装を提供する[`volo-http`][volo-http]クレート。
5. ThriftおよびProtobufコードを生成する[`volo-build`][volo-build]クレート。
6. 新しいプロジェクトをブートストラップし、IDLファイルを管理するCLIインターフェースを提供する[`volo-cli`][volo-cli]クレート。
7. フレームワークのマクロを提供する[`volo-macros`][volo-macros]クレート。

### 特徴

#### AFITとRPITITによって強化

Voloは、AFITとRPITITによって強化されたミドルウェア抽象として[`Motore`][motore]を使用します。

RPITITを通じて、多くの不要な`Box`メモリアロケーションを回避し、使いやすさを向上させ、ユーザーによりフレンドリーなプログラミングインターフェースとより人間工学に基づいたプログラミングパラダイムを提供します。

#### 高性能

Rustはその高性能と安全性で知られています。私たちは設計と実装の過程で常に高性能を目標とし、各場所のオーバーヘッドを可能な限り削減し、各実装のパフォーマンスを向上させます。

まず第一に、Goフレームワークとの性能比較は非常に不公平であるため、VoloとKitexの性能を比較することには重点を置きません。私たちが提供するデータは参考程度にしかなりませんので、皆さんが客観的に見ることを願っています。同時に、オープンソースコミュニティでは他の成熟したRust非同期バージョンのThrift RPCフレームワークが見つからなかったため、性能データの比較をできるだけ弱めたいと考えています。私たちは自分たちのQPSデータのみを公開します。

Kitexと同じテスト条件（4Cに制限）で、VoloのQPSは350kです。同時に、Monoio（CloudWeGoのオープンソースRust非同期ランタイム）に基づくバージョンを内部で検証しており、QPSは440kに達することができます。

私たちのオンラインビジネスのフレームグラフから、Rustの静的分散と優れたコンパイル最適化のおかげで、フレームワーク部分のオーバーヘッドは基本的に無視できることがわかります（syscallオーバーヘッドを除く）。

#### 使いやすさ

~~Rustは学びにくく使いにくいことで知られています~~。私たちは、ユーザーがVoloフレームワークを使用し、Rust言語でマイクロサービスを記述することをできるだけ簡単にし、最も人間工学的で直感的なコーディング体験を提供したいと考えています。したがって、使いやすさを最も重要な目標の1つとしています。

たとえば、プロジェクトのブートストラップとIDLファイルの管理のためのvoloコマンドラインツールを提供しています。同時に、ThriftとgRPCを2つの独立した（ただし一部のコンポーネントを共有する）フレームワークに分割し、異なるプロトコルのセマンティクスとインターフェースに最も適したプログラミングパラダイムを提供します。

また、`#[service]`マクロ（`Box`を必要としない`async_trait`と理解できます）を提供し、ユーザーが心理的な負担なく非同期Rustを使用してサービスミドルウェアを記述できるようにします。

#### 強力な拡張性

Rustの強力な表現力と抽象化能力のおかげで、柔軟なミドルウェア`Service`抽象を通じて、開発者は非常に統一された形式でRPCメタ情報、リクエスト、およびレスポンスを処理できます。

たとえば、サービスディスカバリや負荷分散などのサービスガバナンス機能は、Traitを独自に実装する必要なく、サービスの形式で実装できます。

また、[`Volo-rs`][volo-rs]という組織を作成しました。どんな貢献も歓迎します。

詳細については、[ガイド](https://www.cloudwego.io/zh/docs/volo/guide/)を参照してください。

## チュートリアル

Volo-Thrift: <https://www.cloudwego.io/zh/docs/volo/volo-thrift/getting-started/>

Volo-gRPC: <https://www.cloudwego.io/zh/docs/volo/volo-grpc/getting-started/>

Volo-HTTP: 作業中

## 例

[Examples][examples]を参照してください。

## 関連プロジェクト

- [Volo-rs][volo-rs]: 多くの有用なコンポーネントを含むVoloエコシステム。
- [Motore][motore]: AFITとRPITITによって強化されたミドルウェア抽象層。
- [Pilota][pilota]: 高性能で拡張性のある純粋なRustによるThriftおよびProtobufの実装。
- [Metainfo][metainfo]: コンポーネント間でメタ情報を伝達する。

## ロードマップ

詳細については、[ROADMAP.md](https://github.com/cloudwego/volo/blob/main/ROADMAP.md)を参照してください。

## 貢献

詳細については、[CONTRIBUTING.md](https://github.com/cloudwego/volo/blob/main/CONTRIBUTING.md)を参照してください。

## ライセンス

VoloはMITライセンスとApache License（バージョン2.0）のデュアルライセンスです。

詳細については、[LICENSE-MIT](https://github.com/cloudwego/volo/blob/main/LICENSE-MIT)および[LICENSE-APACHE](https://github.com/cloudwego/volo/blob/main/LICENSE-APACHE)を参照してください。

## クレジット

いくつかのサードパーティコンポーネントを使用しており、その作業に感謝します。

完全なリストについては、[CREDITS.md](https://github.com/cloudwego/volo/blob/main/CREDITS.md)ファイルを参照してください。

## コミュニティ

- Email: [volo@cloudwego.io](mailto:volo@cloudwego.io)
- メンバーになる方法: [COMMUNITY MEMBERSHIP](https://github.com/cloudwego/community/blob/main/COMMUNITY_MEMBERSHIP.md)
- Issues: [Issues](https://github.com/cloudwego/volo/issues)
- Feishu: [Feishu](https://www.feishu.cn/)でQRコードをスキャンするか、[このリンクをクリック](https://applink.feishu.cn/client/chat/chatter/add_by_link?link_token=b34v5470-8e4d-4c7d-bf50-8b2917af026b)してCloudWeGo Voloユーザーグループに参加してください。

  <img src="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-feishu-user-group.png" alt="Volo user group" width="50%" height="50%" />

[volo-rs]: https://github.com/volo-rs
[motore]: https://github.com/cloudwego/motore
[pilota]: https://github.com/cloudwego/pilota
[metainfo]: https://github.com/cloudwego/metainfo
[volo]: https://docs.rs/volo
[volo-thrift]: https://docs.rs/volo-thrift
[volo-grpc]: https://docs.rs/volo-grpc
[volo-http]: https://docs.rs/volo-http
[volo-build]: https://docs.rs/volo-build
[volo-cli]: https://crates.io/crates/volo-cli
[volo-macros]: https://docs.rs/volo-macros
[examples]: https://github.com/cloudwego/volo/tree/main/examples
