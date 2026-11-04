mod shared_fixtures {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../interop/kitex-streaming/rust-fixtures/src/lib.rs"
    ));
}

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../interop/kitex-streaming");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let fixtures = shared_fixtures::prepare(&root, &output).expect("shared TTStream fixtures");
    volo_build::Builder::thrift()
        .add_service(fixtures.interop)
        .add_service(fixtures.audit)
        .write()
        .unwrap();
}
