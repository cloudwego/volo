fn main() {
    let idls = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("idl");
    println!("cargo:rerun-if-changed=idl");
    volo_build::Builder::thrift()
        .add_service(idls.join("interop.thrift"))
        .add_service(idls.join("audit.thrift"))
        .write()
        .unwrap();
}
