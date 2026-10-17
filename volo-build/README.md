<picture>
  <source media="(prefers-color-scheme: light)" srcset="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-light.png?sanitize=true" />
  <source media="(prefers-color-scheme: dark)" srcset="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-dark.png?sanitize=true" />
  <img alt="Volo" src="https://github.com/cloudwego/volo/raw/main/.github/assets/volo-light.png?sanitize=true" />
</picture>

volo-build compiles thrift and protobuf idl files into rust code at compile-time.

## Example

Usually, if you are using `volo-cli` to generate the code, you don't need to use `volo-build` directly.

If you want to use `volo-build` directly, you can follow the following steps:

First, add `volo-build` to your `Cargo.toml`:

```toml
[build-dependencies]
volo-build = "*" # make sure you use a compatible version with `volo`
```

Second, create a `build.rs` file:

```rust,ignore
fn main() {
    volo_build::ConfigBuilder::default().write().unwrap();
}
```

Third, create a `volo.yml` file in the same directory as `build.rs` with the following layout:

```yaml
---
entries:
  thrift:
    filename: thrift_gen.rs
    protocol: thrift
    repos:
      volo:
        url: https://github.com/cloudwego/volo.git
        ref: main
        lock: 58a9eebc4941eb2090c8a07ea142bf073f3527c9
    services:
      - idl:
          source: local
          path: path/to/your/idl.thrift
      - idl:
          source: git
          repo: volo
          path: path/in/repo/idl.thrift
  protobuf:
    filename: protobuf_gen.rs
    protocol: protobuf
    services:
      - idl:
          source: local
          path: path/to/your/protobuf/idl.proto
          includes:
            - path/to/your/protobuf
```

See the [configuration file format guide](https://www.cloudwego.io/docs/volo/guide/config/) for all available options.

That's it!
