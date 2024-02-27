#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

echo_and_run() {
    echo "Running \`$@\`..."
    "$@"
}

echo_and_run cargo fmt -- --check

echo_and_run cargo clippy -p volo-thrift --no-default-features -- --deny warnings
echo_and_run cargo clippy -p volo-thrift --no-default-features --features multiplex -- --deny warnings
echo_and_run cargo clippy -p volo-thrift --no-default-features --features unsafe-codec -- --deny warnings
echo_and_run cargo clippy -p volo-grpc --no-default-features -- --deny warnings
echo_and_run cargo clippy -p volo-grpc --no-default-features --features rustls -- --deny warnings
echo_and_run cargo clippy -p volo-grpc --no-default-features --features native-tls -- --deny warnings
echo_and_run cargo clippy -p volo-grpc --no-default-features --features native-tls-vendored -- --deny warnings
echo_and_run cargo clippy -p volo-http --no-default-features -- --deny warnings
echo_and_run cargo clippy -p volo-http --no-default-features --features default_client -- --deny warnings
echo_and_run cargo clippy -p volo-http --no-default-features --features default_server -- --deny warnings
echo_and_run cargo clippy -p volo-http --no-default-features --features client,server,serde_json -- --deny warnings
echo_and_run cargo clippy -p volo-http --no-default-features --features full -- --deny warnings
echo_and_run cargo clippy -- --deny warnings

echo_and_run cargo test
