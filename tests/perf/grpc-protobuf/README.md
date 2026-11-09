# Public Protobuf regression workload

This standalone binary uses the same pre-existing public codec/transport APIs
against a baseline checkout and a candidate checkout. It runs real TCP or UDS
unary RPCs with identity compression, one shared client, and no hook/deadline.
It does not compare Thrift payload throughput with Protobuf throughput.

Build both copies with the same toolchain, Cargo.lock, profile and dependency
overrides. Only the Volo source paths differ. Run alternating baseline/candidate
processes at least three times per configuration, without concurrent builds or
load tests. Preserve every sample and both binary hashes. The bounded latency
ring is a recent sample, not a full-run histogram.

```sh
grpc-protobuf-load --transport tcp --seconds 5 --concurrency 32 --payload-bytes 1024
```

This is a throughput/latency regression workload. Shutdown diagnostics are
reported separately and are not a substitute for the lifecycle/resource suite.
