//! Run in a directory containing volo.workspace.yml and Cargo.toml.
fn main() {
    if std::env::args().any(|arg| arg == "protobuf") {
        volo_build::workspace::Builder::protobuf()
            .plugin(volo_streamx_build::plugin())
            .r#gen();
    } else {
        volo_build::workspace::Builder::thrift()
            .plugin(volo_streamx_build::plugin())
            .r#gen();
    }
}
