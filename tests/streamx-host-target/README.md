# StreamX host/target consumers

This independent workspace contains two consumers of the same Thrift and
Protobuf inputs. `boundary-ordinary` installs only the existing `volo-build`
builder. `boundary-streamx` adds `volo-streamx-build` to build-dependencies and
`volo-streamx` to normal dependencies. Both inherit dependencies from the root,
generate split files, and resolve messages across includes/imports. The StreamX
consumer compiles real handlers and server registrations for both IDLs.

Run from the Volo checkout. The companion Pilota branch is selected in this
workspace manifest and pinned by its lockfile:

```sh
cargo +nightly check --locked --manifest-path tests/streamx-host-target/Cargo.toml \
  --target x86_64-unknown-linux-gnu -p boundary-ordinary
cargo +nightly check --locked --manifest-path tests/streamx-host-target/Cargo.toml \
  --target x86_64-unknown-linux-gnu -p boundary-streamx
rustup target add aarch64-unknown-linux-gnu --toolchain nightly
cargo +nightly check --locked --manifest-path tests/streamx-host-target/Cargo.toml \
  --target aarch64-unknown-linux-gnu -p boundary-streamx
cargo +nightly tree --locked --manifest-path tests/streamx-host-target/Cargo.toml \
  --edges normal --target aarch64-unknown-linux-gnu -p boundary-streamx
```

Each build script records the actual `HOST` and `TARGET` in
`OUT_DIR/build-targets.txt`. Cargo's JSON artifact output distinguishes host
generator libraries/build scripts from target runtime libraries, even when both
targets use the same compiler. Inspect normal dependency closures separately
from build closures; existing proc-macro dependencies on `syn` are expected.
Cross `cargo check` verifies compilation, without claiming target execution.

For generated multi-crate Workspace output, build the existing generator and
use a fresh directory. The two consumer sources compile handlers whose request
types come from the included parent or imported common-message crate:

```sh
cargo +nightly build --locked -p volo-streamx-build --example workspace
python3 tests/streamx-host-target/make_workspace.py thrift /tmp/boundary-thrift \
  --generator target/debug/examples/workspace
python3 tests/streamx-host-target/make_workspace.py protobuf /tmp/boundary-protobuf \
  --generator target/debug/examples/workspace
# Supply the same candidate patch configuration when checking outside this checkout.
cargo +nightly check --config /path/to/candidate.toml \
  --manifest-path /tmp/boundary-thrift/Cargo.toml --workspace \
  --target aarch64-unknown-linux-gnu
```

Workspace generation runs as a host CLI; generated members inherit runtime
dependencies and contain no build-plugin dependency. The first pair of consumers
separately exercises actual build.rs host/target isolation.

For descriptive clean-build costs, use an empty, separate `--target-dir` for
each consumer and target, fixed `CARGO_BUILD_JOBS`, and
`CARGO_INCREMENTAL=0`. Keep the lock and candidate sources unchanged, record the
command, wall time and Cargo artifact output, and run cases serially. These
compile costs do not substitute for runtime throughput and latency measurements.
