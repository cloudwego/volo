#!/bin/bash
set -e
CURDIR=$(cd $(dirname $0); pwd)

# clean
if [ -z "$output_dir" ]; then
  echo "output_dir is empty"
  exit 1
fi
rm -rf $output_dir/bin/ && mkdir -p $output_dir/bin/
rm -rf $output_dir/log/ && mkdir -p $output_dir/log/

# build clients
cargo build --bin bench-client --features unsafe-codec --release
cp $CURDIR/../../target/release/bench-client $output_dir/bin/bench-client

# build servers
cargo build --bin bench-server --features unsafe-codec --release
cp $CURDIR/../../target/release/bench-server $output_dir/bin/bench-server
