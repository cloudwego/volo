#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

echo_command() {
	echo "$@"

	if [ "${GITHUB_ACTIONS:-}" = "true" ] || [ -n "${DEBUG:-}" ]; then
		# If we are in GitHub Actions or env `DEBUG` is non-empty,
		# output all
		"$@"
	else
		trap 'echo -e "\e[1;31merror:\e[0m failed to run: $@"' ERR
		# Disable outputs
		"$@" > /dev/null 2>&1
	fi
}

run_clippy() {
	echo_command cargo clippy -p volo-thrift --no-default-features -- --deny warnings
	echo_command cargo clippy -p volo-thrift --no-default-features --features multiplex -- --deny warnings
	echo_command cargo clippy -p volo-thrift --no-default-features --features unsafe-codec -- --deny warnings
	echo_command cargo clippy -p volo-grpc --no-default-features -- --deny warnings
	echo_command cargo clippy -p volo-grpc --no-default-features --features rustls -- --deny warnings
	echo_command cargo clippy -p volo-grpc --no-default-features --features native-tls -- --deny warnings
	echo_command cargo clippy -p volo-grpc --no-default-features --features native-tls-vendored -- --deny warnings
	echo_command cargo clippy -p volo-grpc --no-default-features --features grpc-web -- --deny warnings
	echo_command cargo clippy -p volo-http -- --deny warnings
	echo_command cargo clippy -p volo-http --no-default-features --features client,http1,json -- --deny warnings
	echo_command cargo clippy -p volo-http --no-default-features --features client,http2,json -- --deny warnings
	echo_command cargo clippy -p volo-http --no-default-features --features server,http1,query,form,json,multipart,ws -- --deny warnings
	echo_command cargo clippy -p volo-http --no-default-features --features server,http2,query,form,json,multipart,ws -- --deny warnings
	echo_command cargo clippy -p volo-http --no-default-features --features full -- --deny warnings
	echo_command cargo clippy -p volo -- --deny warnings
	echo_command cargo clippy -p volo --no-default-features --features rustls-aws-lc-rs -- --deny warnings
	echo_command cargo clippy -p volo --no-default-features --features rustls-ring -- --deny warnings
	echo_command cargo clippy -p volo-build -- --deny warnings
	echo_command cargo clippy -p volo-cli -- --deny warnings
	echo_command cargo clippy -p volo-macros -- --deny warnings
	echo_command cargo clippy -p examples -- --deny warnings
	echo_command cargo clippy -p examples --features tls -- --deny warnings
	echo_command cargo clippy --all -- --deny warnings
}

run_test() {
	echo_command cargo test -p volo-thrift
	echo_command cargo test -p volo-grpc --features rustls
	echo_command cargo test -p volo-http --features client,server,http1,query,form,json,tls,cookie,multipart,ws
	echo_command cargo test -p volo-http --features client,server,http2,query,form,json,tls,cookie,multipart,ws
	echo_command cargo test -p volo-http --features full
	echo_command cargo test -p volo --features rustls
	echo_command cargo test -p volo-build
	echo_command cargo test -p volo-cli
}

main() {
	local RUN_CLIPPY="yes"
	local RUN_TEST="yes"

	for arg in "$@"; do
		case "${arg}" in
		--no-clippy)
			RUN_CLIPPY="no"
			echo "info: clippy checks will be ignored"
			;;
		--no-test)
			RUN_TEST="no"
			echo "info: unit tests will be ignored"
			;;
		esac
	done

	if [ "${RUN_CLIPPY}" = "yes" ]; then
		run_clippy
	fi
	if [ "${RUN_TEST}" = "yes" ]; then
		run_test
	fi
}

main "$@"
