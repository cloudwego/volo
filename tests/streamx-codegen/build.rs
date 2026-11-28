fn main() {
    println!("cargo:rerun-if-changed=idl/arc_shapes.thrift");
    volo_build::Builder::thrift()
        .add_service("idl/arc_shapes.thrift")
        .plugin(volo_streamx_build::plugin())
        .filename("arc_shapes_thrift.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::thrift_grpc()
        .add_service("idl/arc_shapes.thrift")
        .plugin(volo_streamx_build::plugin())
        .thrift_grpc_options(volo_build::thrift_grpc_backend::Options {
            shared_types_module: Some("crate::arc_shapes_thrift".into()),
            ..Default::default()
        })
        .filename("arc_shapes_grpc.rs".into())
        .write()
        .unwrap();

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("idl");
    println!("cargo:rerun-if-changed=idl/oneway.thrift");
    volo_build::Builder::thrift()
        .add_service(root.join("oneway.thrift"))
        .plugin(volo_streamx_build::plugin())
        .filename("oneway.rs".into())
        .write()
        .unwrap();
    let names = root.clone();
    let edges = names.join("name_edges");
    for name in [
        "parent.thrift",
        "renamed.thrift",
        "no_namespace.thrift",
        "my-service.thrift",
        "no_package.proto",
    ] {
        println!("cargo:rerun-if-changed={}", edges.join(name).display());
    }
    volo_build::Builder::thrift()
        .add_service(edges.join("renamed.thrift"))
        .add_service(edges.join("no_namespace.thrift"))
        .add_service(edges.join("my-service.thrift"))
        .plugin(volo_streamx_build::plugin())
        .filename("name_edges_thrift.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::thrift_grpc()
        .add_service(edges.join("renamed.thrift"))
        .add_service(edges.join("no_namespace.thrift"))
        .add_service(edges.join("my-service.thrift"))
        .plugin(volo_streamx_build::plugin())
        .thrift_grpc_options(volo_build::thrift_grpc_backend::Options {
            shared_types_module: Some("crate::name_edges_thrift".into()),
            ..Default::default()
        })
        .filename("name_edges_grpc.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::protobuf()
        .add_service(edges.join("no_package.proto"))
        .include_dirs(vec![edges])
        .plugin(volo_streamx_build::plugin())
        .filename("name_edges_pb.rs".into())
        .write()
        .unwrap();
    for name in [
        "alpha.proto",
        "beta.proto",
        "echo.thrift",
        "conflicts.thrift",
        "override.thrift",
        "presence.proto",
    ] {
        println!("cargo:rerun-if-changed={}", names.join(name).display());
    }
    volo_build::Builder::protobuf()
        .add_service(names.join("alpha.proto"))
        .add_service(names.join("beta.proto"))
        .include_dirs(vec![names.clone()])
        .plugin(volo_streamx_build::plugin())
        .filename("names_pb.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::thrift()
        .add_service(names.join("echo.thrift"))
        .add_service(names.join("conflicts.thrift"))
        .plugin(volo_streamx_build::plugin())
        .filename("names_thrift.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::thrift_grpc()
        .add_service(names.join("override.thrift"))
        .plugin(volo_streamx_build::plugin())
        .thrift_grpc_options(volo_build::thrift_grpc_backend::Options {
            wire_services: [("Renamed".into(), "custom.Endpoint".into())].into(),
            ..Default::default()
        })
        .filename("names_override.rs".into())
        .write()
        .unwrap();
    for name in ["chat.thrift", "parent.thrift", "presence.proto"] {
        println!("cargo:rerun-if-changed=idl/{name}");
    }
    volo_build::Builder::thrift()
        .add_service(root.join("chat.thrift"))
        .plugin(volo_streamx_build::plugin())
        .filename("thrift.rs".into())
        .write()
        .unwrap();
    let client_idl = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("client.thrift");
    std::fs::write(
        &client_idl,
        include_str!("idl/chat.thrift")
            .lines()
            .filter(|line| !line.contains("oneway void Fire"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    volo_build::Builder::thrift_grpc()
        .add_service(client_idl)
        .include_dirs(vec![root.clone()])
        .plugin(volo_streamx_build::plugin())
        .thrift_grpc_options(volo_build::thrift_grpc_backend::Options {
            shared_types_module: Some("crate::thrift".into()),
            ..Default::default()
        })
        .filename("grpc.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::protobuf()
        .add_service(names.join("presence.proto"))
        .include_dirs(vec![names.clone()])
        .plugin(volo_streamx_build::plugin())
        .filename("protobuf.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::thrift()
        .add_service(root.join("chat.thrift"))
        .plugin(volo_streamx_build::plugin())
        .split_generated_files(true)
        .filename("split_thrift.rs".into())
        .write()
        .unwrap();
    volo_build::Builder::protobuf()
        .add_service(names.join("presence.proto"))
        .include_dirs(vec![names])
        .plugin(volo_streamx_build::plugin())
        .split_generated_files(true)
        .filename("split_protobuf.rs".into())
        .write()
        .unwrap();
}
