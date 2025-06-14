#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

quiet() {
	"$@" > /dev/null 2>&1
}

echo_command() {
	echo "$@"

	if [ "${GITHUB_ACTIONS:-}" = "true" ] || [ -n "${DEBUG:-}" ]; then
		# If we are in GitHub Actions or env `DEBUG` is non-empty,
		# output all
		"$@"
	else
		trap 'echo -e "\e[1;31merror:\e[0m failed to run: $@"' ERR
		# Disable outputs
		quiet "$@"
	fi
}

enter_tmp_dir() {
	export __TMP_DIR="$(mktemp --tmpdir --directory volo_cli.XXXXXX)"
	quiet pushd "${__TMP_DIR}"
}

escape_tmp_dir() {
	quiet popd
	rm -rf "${__TMP_DIR}"
	unset __TMP_DIR
}

init() {
	export VOLO_DIR="$PWD"
	echo_command cargo build -p volo-cli
	export VOLO_CLI="$PWD/target/debug/volo"
}

append_volo_dep_item() {
	echo "$1 = { path = \"$VOLO_DIR/$1\" }" >> Cargo.toml
}

append_pilota_dep_item() {
	echo "$1 = { git = \"https://github.com/cloudwego/pilota.git\", branch = \"main\" }" >> Cargo.toml
}

patch_cargo_toml() {
	echo "[patch.crates-io]" >> Cargo.toml

	append_volo_dep_item volo
	append_volo_dep_item volo-build
	append_volo_dep_item volo-thrift
	append_volo_dep_item volo-grpc
	append_volo_dep_item volo-http

	append_pilota_dep_item pilota
	append_pilota_dep_item pilota-build
	append_pilota_dep_item pilota-thrift-parser
}

thrift_test() {
	local idl_path="$VOLO_DIR/examples/thrift/echo.thrift"

	enter_tmp_dir

	echo_command "${VOLO_CLI}" init thrift-test "${idl_path}"
	patch_cargo_toml
	echo_command cargo build

	escape_tmp_dir
}

grpc_test() {
	local idl_path="$VOLO_DIR/examples/proto/echo.proto"
	local idl_dir="$(dirname "${idl_path}")"

	enter_tmp_dir

	echo_command "${VOLO_CLI}" init --includes "${idl_dir}" grpc-test "${idl_path}"
	patch_cargo_toml
	echo_command cargo build

	escape_tmp_dir
}

http_test() {
	enter_tmp_dir

	echo_command "${VOLO_CLI}" http init http-test
	patch_cargo_toml
	echo_command cargo build

	escape_tmp_dir
}

main() {
	init
	thrift_test
	grpc_test
	http_test
}

main
