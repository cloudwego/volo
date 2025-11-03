#!/bin/bash

set -o errexit
set -o nounset
set -o pipefail

IGNORE_REGEX='(^|[\\/])(benches|tests?|examples|target|gen|test_data)([\\/])|(^|[\\/])build\\.rs$|(^|[\\/])\\.cargo[\\/]registry'

run_nextest() {
  local package="$1"
  local features="${2:-}"
  local no_tests_behavior="${NO_TESTS_BEHAVIOR:-pass}"
  if [ -n "$features" ]; then
    cargo llvm-cov nextest -p "$package" --features "$features" --all-targets --no-report \
      --no-tests="$no_tests_behavior" \
      --ignore-filename-regex "$IGNORE_REGEX"
  else
    cargo llvm-cov nextest -p "$package" --all-targets --no-report \
      --no-tests="$no_tests_behavior" \
      --ignore-filename-regex "$IGNORE_REGEX"
  fi
}

report_all() {
  cargo llvm-cov report --lcov --output-path lcov.info \
    --ignore-filename-regex "$IGNORE_REGEX"
  cargo llvm-cov report --html \
    --ignore-filename-regex "$IGNORE_REGEX"
}

main() {
  # 1. clean up previous coverage data
  cargo llvm-cov clean --workspace || true

  # 2.run tests with coverage, align with scripts/clippy-and-test.sh
  run_nextest volo-thrift
  run_nextest volo-grpc 'rustls'
  run_nextest volo-http 'client,server,http1,query,form,json,tls,cookie,multipart,ws'
  run_nextest volo-http 'client,server,http2,query,form,json,tls,cookie,multipart,ws'
  run_nextest volo-http 'full'
  run_nextest volo 'rustls'
  run_nextest volo-build
  run_nextest volo-cli

  # 3.generate coverage report
  report_all
}

main "$@"


