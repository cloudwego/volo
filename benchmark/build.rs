use std::path::PathBuf;

fn main() {
    volo_build::Builder::thrift()
        .add_service("idl/echo.thrift")
        .filename(PathBuf::from("benchmark.rs"))
        .write()
        .unwrap()
}
