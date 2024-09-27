#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

echo_and_run() {
	echo "Running $@"

	if [ -n "${DEBUG:-}" ]; then
		# If env `DEBUG` is non-empty, output all
		"$@"
	else
		# Disable outputs
		"$@" > /dev/null 2>&1
	fi
}

fmt_check() {
	echo_and_run cargo fmt -- --check
}

docs_check() {
	echo_and_run cargo rustdoc -p volo --all-features -- --deny warnings
	echo_and_run cargo rustdoc -p volo-build --all-features -- --deny warnings
	echo_and_run cargo rustdoc -p volo-grpc --all-features -- --deny warnings
	echo_and_run cargo rustdoc -p volo-http --all-features -- --deny warnings
	echo_and_run cargo rustdoc -p volo-thrift --all-features -- --deny warnings
}

clippy_and_test() {
	bash "scripts/clippy-and-test.sh"
}

volo_cli_test() {
	bash "scripts/volo-cli-test.sh"
}

main() {
	fmt_check
	docs_check
	clippy_and_test
	volo_cli_test
}

main
