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

fmt_check() {
	echo_and_run cargo fmt -- --check
}

clippy_and_test() {
	bash "scripts/clippy-and-test.sh"
}

volo_cli_test() {
	bash "scripts/volo-cli-test.sh"
}

main() {
	fmt_check
	clippy_and_test
	volo_cli_test
}

main
