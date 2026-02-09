# CLAUDE.md - volo-build

## Overview

`volo-build` is the code generation tool for the Volo framework, compiling Thrift and Protobuf IDL files into Rust code at build time. It is the underlying dependency of `volo-cli` and is typically used in `build.rs`.

## Directory Structure

```
volo-build/src/
├── lib.rs              # Library entry, defines Builder and public exports
├── model.rs            # Configuration model (SingleConfig, Entry, Service, Idl, etc.)
├── config_builder.rs   # ConfigBuilder and InitBuilder
├── thrift_backend.rs   # Thrift code generation backend
├── grpc_backend.rs     # gRPC/Protobuf code generation backend
├── util.rs             # Git operations, file operations, config read/write
├── workspace.rs        # Workspace mode support
└── legacy/             # Legacy configuration format compatibility
```

## Builder (`lib.rs`)

Main code generation builder supporting both Thrift and Protobuf protocols. Created via `Builder::thrift()` or `Builder::protobuf()`.

Key methods: `add_service(path)`, `out_dir(path)`, `filename(name)`, `plugin(p)`, `ignore_unused(bool)`, `touch(items)`, `keep_unknown_fields(paths)`, `split_generated_files(bool)`, `special_namings(namings)`, `dedup(list)`, `common_crate_name(name)`, `with_descriptor(bool)`, `with_field_mask(bool)`, `with_comments(bool)`, `include_dirs(dirs)`, `write()`, `init_service()`.

## ConfigBuilder (`config_builder.rs`)

Configuration file-based (`volo.yml`) code generation builder. Use `ConfigBuilder::default().write()` for the default config file, or `ConfigBuilder::new(path)` for a custom one. Supports adding plugins via `.plugin(p)`.

Also provides `InitBuilder` for initializing new services.

## Configuration Model (`model.rs`)

`SingleConfig` is the root structure. Key types: `Entry` (code generation entry), `Service` (service definition with IDL and codegen options), `Idl` (IDL file source and path), `Source` (Local or Git), `Repo` (Git repository config), `CodegenOption`, `CommonOption`.

Example `volo.yml`:

```yaml
entries:
  default:
    filename: volo_gen.rs
    protocol: thrift
    repos:
      my_repo:
        url: https://github.com/example/idl.git
        ref: main
        lock: abc123
    services:
      - idl:
          source: local
          path: ./idl/service.thrift
        codegen_option:
          touch: ["MyService"]
    touch_all: false
    dedups: []
    special_namings: []
    split_generated_files: false
```

## Thrift Backend (`thrift_backend.rs`)

Implements `pilota_build::CodegenBackend` for Thrift services. Generates: `{ServiceName}Server`, `{ServiceName}Client`, `{ServiceName}GenericClient`, `{ServiceName}OneShotClient`, `{ServiceName}ClientBuilder`, `{ServiceName}RequestSend/Recv`, `{ServiceName}ResponseSend/Recv`. Supports exception handling, oneway methods, multi-service routing, and split file generation.

## gRPC Backend (`grpc_backend.rs`)

Implements `pilota_build::CodegenBackend` for gRPC services. Generates the same type pattern as Thrift (`Server`, `Client`, `GenericClient`, `OneShotClient`, `ClientBuilder`, `RequestSend/Recv`, `ResponseSend/Recv`). Supports client streaming, server streaming, and bidirectional streaming.

## Workspace Support (`workspace.rs`)

Supports code generation for multi-crate workspaces via `volo.workspace.yml`. Use `workspace::Builder::thrift().gen()` or `workspace::Builder::protobuf().gen()`.

## Notes

1. **OUT_DIR**: Must be run in `build.rs`; depends on the `OUT_DIR` environment variable.
2. **Git Operations**: Requires system `git` CLI installed.
3. **Config Migration**: Legacy configuration formats need migration to the new format. See https://www.cloudwego.io/docs/volo/guide/config/
4. **Split Files**: Enabling `split_generated_files` generates multiple files, suitable for large projects.
