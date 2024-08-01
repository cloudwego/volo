#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

echo_command() {
	echo "Run \`$@\`"

	if [ "${GITHUB_ACTIONS:-}" = "true" ] || [ -n "${DEBUG:-}" ]; then
		# If we are in GitHub Actions or env `DEBUG` is non-empty,
		# output all
		"$@"
	else
		# Disable outputs
		"$@" > /dev/null 2>&1
	fi
}

# Setup error handler
trap 'echo "Failed to run $LINENO: $BASH_COMMAND (exit code: $?)" && exit 1' ERR

# Clippy
echo_command cargo clippy -p volo-thrift --no-default-features -- --deny warnings
echo_command cargo clippy -p volo-thrift --no-default-features --features multiplex -- --deny warnings
echo_command cargo clippy -p volo-thrift --no-default-features --features unsafe-codec -- --deny warnings
echo_command cargo clippy -p volo-grpc --no-default-features -- --deny warnings
echo_command cargo clippy -p volo-grpc --no-default-features --features rustls -- --deny warnings
echo_command cargo clippy -p volo-grpc --no-default-features --features native-tls -- --deny warnings
echo_command cargo clippy -p volo-grpc --no-default-features --features native-tls-vendored -- --deny warnings
echo_command cargo clippy -p volo-grpc --no-default-features --features grpc-web -- --deny warnings
echo_command cargo clippy -p volo-http --no-default-features -- --deny warnings
echo_command cargo clippy -p volo-http --no-default-features --features default_client -- --deny warnings
echo_command cargo clippy -p volo-http --no-default-features --features default_server -- --deny warnings
echo_command cargo clippy -p volo-http --no-default-features --features client,server -- --deny warnings
echo_command cargo clippy -p volo-http --no-default-features --features full -- --deny warnings
echo_command cargo clippy -p volo -- --deny warnings
echo_command cargo clippy -p volo-build -- --deny warnings
echo_command cargo clippy -p volo-cli -- --deny warnings
echo_command cargo clippy -p volo-macros -- --deny warnings
echo_command cargo clippy -p examples -- --deny warnings

# Test
echo_command cargo test -p volo-thrift
echo_command cargo test -p volo-grpc --features rustls
echo_command cargo test -p volo-http --features default_client,default_server
echo_command cargo test -p volo-http --features full
echo_command cargo test -p volo --features rustls
echo_command cargo test -p volo-build
echo_command cargo test -p volo-cli
