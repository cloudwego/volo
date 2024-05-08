#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

echo_and_run() {
	echo "Running \`$@\`..."
	"$@"
}

quiet() {
	"$@" > /dev/null 2>&1
}

test_in_empty_dir() {
	local tmp_dir="$(mktemp --tmpdir --directory volo_cli.XXXX)"
	quiet pushd "${tmp_dir}"

	echo_and_run "$@"

	quiet popd
	rm -rf "${tmp_dir}"
}

fmt_check() {
	echo_and_run cargo fmt -- --check
}

clippy_check() {
	# check and clippy for all crates and common features
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
	echo_and_run cargo clippy -p volo -- --deny warnings
	echo_and_run cargo clippy -p volo-build -- --deny warnings
	echo_and_run cargo clippy -p volo-cli -- --deny warnings
	echo_and_run cargo clippy -p volo-macros -- --deny warnings
	echo_and_run cargo clippy -p examples -- --deny warnings
}

unit_test() {
	echo_and_run cargo test -p volo-thrift
	echo_and_run cargo test -p volo-grpc --features rustls
	echo_and_run cargo test -p volo-http --features full
	echo_and_run cargo test -p volo -- features rustls
	echo_and_run cargo test -p volo-build
	echo_and_run cargo test -p volo-cli
}

volo_cli_test() {
	cargo build -p volo-cli -j `nproc`
	local volo_cli="$PWD/target/debug/volo"
	local thrift_idl="$PWD/examples/thrift_idl/echo.thrift"
	local pb_idl="$PWD/examples/proto/echo.proto"
	local pb_idl_filename="$(basename "${pb_idl}")"

	# thrift
	test_in_empty_dir bash -c "\
\"${volo_cli}\" init thrift-test \"${thrift_idl}\" && \
cargo build -j \`nproc\`"

	# grpc
	test_in_empty_dir bash -c "\
mkdir idl && cp \"${pb_idl}\" idl && \
\"${volo_cli}\" init --includes idl grpc-test \"idl/${pb_idl_filename}\" && \
cargo build -j \`nproc\`"

	# http
	test_in_empty_dir bash -c "\
\"${volo_cli}\" http init http-test && \
cargo build -j \`nproc\`"
}

main() {
	fmt_check
	clippy_check
	unit_test
	volo_cli_test
}

main
