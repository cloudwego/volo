fn main() {
    volo_build::ConfigBuilder::default().write().unwrap();

    volo_build::Builder::thrift()
        .add_service("../thrift_idl/echo_unknown.thrift")
        .keep_unknown_fields(true)
        .write()
        .unwrap();
}
