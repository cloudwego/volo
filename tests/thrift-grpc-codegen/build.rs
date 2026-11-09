fn main() {
    let idls =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../interop/kitex-streaming/go/idl");
    for name in [
        "grpc_parent.thrift",
        "grpc_thrift.thrift",
        "grpc_no_namespace.thrift",
    ] {
        println!("cargo:rerun-if-changed={}", idls.join(name).display());
    }
    volo_build::Builder::thrift_grpc()
        .add_service(idls.join("grpc_thrift.thrift"))
        .add_service(idls.join("grpc_no_namespace.thrift"))
        .add_service(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("idl/shapes.thrift"))
        .ignore_unused(false)
        .write()
        .unwrap();
    // Both protocol modules refer to precisely the same Pilota message types.
    let shared_wrappers =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("idl/shared_wrappers.thrift");
    println!("cargo:rerun-if-changed={}", shared_wrappers.display());
    volo_build::Builder::thrift()
        .add_service(idls.join("grpc_thrift.thrift"))
        .add_service(shared_wrappers.clone())
        .filename("ttstream.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::thrift_grpc()
        .add_service(idls.join("grpc_thrift.thrift"))
        .add_service(shared_wrappers)
        .thrift_grpc_options(volo_build::thrift_grpc_backend::Options {
            shared_types_module: Some("crate::ttstream".into()),
            ..Default::default()
        })
        .filename("shared.rs".into())
        .write()
        .unwrap();
}
