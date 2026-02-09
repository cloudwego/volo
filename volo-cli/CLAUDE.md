# CLAUDE.md - volo-cli

## Project Overview

`volo-cli` is the command line tool for the Volo framework, used for creating and managing Volo-based RPC and HTTP service projects from IDL files or templates.

## Directory Structure

```
volo-cli/
└── src/
    ├── bin/
    │   └── volo.rs         # CLI entry point (main function)
    ├── lib.rs              # Library root, defines macros and exports modules
    ├── command.rs          # CliCommand trait and define_commands! macro
    ├── context.rs          # Context struct
    ├── model.rs            # RootCommand and subcommand definitions
    ├── init.rs             # `volo init` command implementation
    ├── http.rs             # `volo http` command implementation
    ├── migrate.rs          # `volo migrate` command implementation
    ├── idl/
    │   ├── mod.rs          # `volo idl` command entry
    │   └── add.rs          # `volo idl add` subcommand
    ├── repo/
    │   ├── mod.rs          # `volo repo` command entry
    │   ├── add.rs          # `volo repo add` subcommand
    │   └── update.rs       # `volo repo update` subcommand
    └── templates/          # Project template files
        ├── thrift/         # Thrift project templates
        ├── grpc/           # gRPC project templates
        └── http/           # HTTP project templates
```

## Commands

| Command                    | Module           | Description                                 |
| -------------------------- | ---------------- | ------------------------------------------- |
| `volo init <name> <idl>`   | `init.rs`        | Initialize Thrift/gRPC project              |
| `volo http init <name>`    | `http.rs`        | Initialize HTTP project                     |
| `volo idl add <idl>`       | `idl/add.rs`     | Add IDL file to existing project            |
| `volo repo add -g <git>`   | `repo/add.rs`    | Add Git repository as IDL source            |
| `volo repo update [repos]` | `repo/update.rs` | Update specified or all Git repository IDLs |
| `volo migrate`             | `migrate.rs`     | Migrate legacy configuration to new format  |

## Key Macros

### `define_commands!` (`src/command.rs`)

Batch-defines subcommand enums and auto-implements the `CliCommand` trait, simplifying command dispatch logic. All commands implement the `CliCommand` trait (`fn run(&self, cx: Context) -> anyhow::Result<()>`).

### `templates_to_target_file!` (`src/lib.rs`)

Outputs template files to a target path with parameter substitution:

```rust
templates_to_target_file!(folder, "templates/thrift/cargo_toml", "Cargo.toml", name = &name);
```

## Project Templates

- **Thrift** (`templates/thrift/`): Standard Thrift RPC project with `volo-gen` sub-crate for code generation
- **gRPC** (`templates/grpc/`): Protobuf-based gRPC project, similar structure to Thrift
- **HTTP** (`templates/http/`): Pure HTTP project using `volo-http` Router pattern, no `volo-gen` sub-crate

## Logging

- Default log level is `WARN`
- Use `-v` to raise to `INFO`, `-vv` for `DEBUG`, `-vvv` for `TRACE`
- Version update check runs at startup; disable with env var `VOLO_DISABLE_UPDATE_CHECK`

## Notes

1. The init command checks if `volo.yml` already exists to avoid overwriting existing configuration
2. IDL protocol must be consistent with existing entry configuration (cannot mix Thrift and Protobuf)
3. After initialization, `cargo fmt --all` is automatically run to format generated code
4. After initialization, Git repository is automatically initialized (if not already present)
