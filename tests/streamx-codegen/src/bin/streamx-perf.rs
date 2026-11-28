//! I10 entry using the common measurement process and native server launchers.
pub use streamx_codegen::{grpc, perf_server, protobuf, thrift};

#[path = "../perf_entry.rs"]
mod perf_entry;
#[path = "../perf_runtime.rs"]
mod perf_runtime;

#[tokio::main(worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    perf_entry::run().await
}
