![Volo](https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true)

volo-build compiles thrift and protobuf idl files into rust code at compile-time.

## Example

Usually, if you are using `volo-cli` to generate the code, you don't need to use `volo-build` directly.

If you want to use `volo-build` directly, you can follow the following steps:

First, add `volo-build` to your `Cargo.toml`:

```toml
[build-dependencies]
volo-build = "*" # make sure you use a compatible version with `volo`
```

Second, creates a `build.rs` file:

```rust
fn main() {
    volo_build::Builder::default().write().unwrap();
}
```

Third, creates a `volo.yml` file in the same directory of `build.rs` with the following layout:

```yaml
---
idls:
  - source: local
    path: path/to/your/idl.thrift
  - source: local
    path: path/to/your/protobuf/idl.proto
    includes:
    - path/to/your/protobuf/
  - source: git
    repo: git@github.com:cloudwego/volo.git
    ref: main
    path: path/in/repo/idl.thrift
```

That's it!
