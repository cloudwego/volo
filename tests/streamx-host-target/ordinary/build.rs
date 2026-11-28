use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../idl");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed={}", root.display());
    volo_build::Builder::thrift()
        .add_service(root.join("api.thrift"))
        .split_generated_files(true)
        .out_dir(out.join("thrift"))
        .filename("generated.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::protobuf()
        .add_service(root.join("api.proto"))
        .include_dirs(vec![root])
        .split_generated_files(true)
        .out_dir(out.join("protobuf"))
        .filename("generated.rs".into())
        .write()
        .unwrap();
    std::fs::write(
        out.join("build-targets.txt"),
        format!(
            "host={}\ntarget={}\n",
            std::env::var("HOST").unwrap(),
            std::env::var("TARGET").unwrap()
        ),
    )
    .unwrap();
}
