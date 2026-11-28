use std::fs;

#[test]
fn ordinary_protobuf_clients_and_server_publish_all_method_shapes() {
    let root = tempfile::tempdir().unwrap();
    let idl = root.path().join("probe.proto");
    fs::write(
        &idl,
        r#"
        syntax = "proto3";
        package ordinary.api;
        message Value { string text = 1; }
        service Echo {
            rpc Get(Value) returns (Value);
            rpc Upload(stream Value) returns (Value);
            rpc Download(Value) returns (stream Value);
            rpc Chat(stream Value) returns (stream Value);
        }
    "#,
    )
    .unwrap();
    volo_build::Builder::protobuf()
        .add_service(idl)
        .include_dirs(vec![root.path().to_path_buf()])
        .out_dir(root.path())
        .write()
        .unwrap();
    let code = fs::read_to_string(root.path().join("volo_gen.rs")).unwrap();
    assert!(code.contains("fn descriptor()"));
    assert!(code.contains("PayloadKind::Protobuf"));
    for mode in [
        "Unary",
        "ClientStreaming",
        "ServerStreaming",
        "Bidirectional",
    ] {
        assert_eq!(
            code.matches(&format!("RpcMode::{mode}")).count(),
            3,
            "server plus ordinary/oneshot client: {mode}"
        );
    }
    assert_eq!(code.matches("PayloadShape::ProtobufMessage").count(), 12);
}
