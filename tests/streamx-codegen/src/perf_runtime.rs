//! Native launchers for the shared I10 process. No work runs per RPC here.
pub(crate) const PROFILE: &str = "volo-native";

pub(crate) fn server() -> volo_streamx::Server {
    volo_streamx::Server::new()
}

pub(crate) fn observe_process_resources(_: &mut serde_json::Value) {}

macro_rules! run_mixed {
    ($server:expr, $incoming:expr $(,)?) => {
        ($server).run($incoming)
    };
}
pub(crate) use run_mixed;

macro_rules! run_grpc {
    ($server:expr, $incoming:expr $(,)?) => {
        ($server).run($incoming)
    };
}
pub(crate) use run_grpc;

macro_rules! run_thrift {
    ($server:expr, $incoming:expr $(,)?) => {
        ($server).run($incoming)
    };
}
pub(crate) use run_thrift;
