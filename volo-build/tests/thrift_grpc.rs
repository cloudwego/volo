use std::{fs, path::Path};

use volo_build::{Builder, ConfigBuilder};

fn generate(root: &Path, idl: &str) -> anyhow::Result<String> {
    let input = root.join("fixture.thrift");
    fs::write(&input, idl)?;
    Builder::thrift_grpc()
        .add_service(input)
        .out_dir(root)
        .write()?;
    Ok(fs::read_to_string(root.join("volo_gen.rs"))?)
}

#[test]
fn oneway_is_rejected_before_emitting_any_files() {
    let root = tempfile::tempdir().unwrap();
    let error = generate(root.path(), "service One { oneway void Notify() }").unwrap_err();
    assert!(error.to_string().contains("oneway method One.Notify"));
    assert!(!root.path().join("volo_gen.rs").exists());
}

#[test]
fn streaming_throws_in_any_service_rejects_the_entry() {
    let root = tempfile::tempdir().unwrap();
    let error = generate(root.path(), r#"
        struct Message {}
        exception Failure { 1: string reason }
        service Good { Message Call(1: Message request) }
        service Unused { Message Watch(1: Message request) throws (1: Failure failure) (streaming.mode="server") }
    "#).unwrap_err();
    assert!(error.to_string().contains("throws"));
    assert!(!root.path().join("volo_gen.rs").exists());
}

#[test]
fn wire_names_and_method_descriptors_ignore_rust_renaming() {
    let root = tempfile::tempdir().unwrap();
    let code = generate(root.path(), r#"
        namespace go first.second.api
        namespace rs my_rust_module
        struct Message {}
        service WireName {
            Message OriginalCase(1: Message request) (streaming.mode="server", pilota.name="rust_method")
            void Zero()
        }
    "#).unwrap();
    assert!(code.contains("/api.WireName/OriginalCase"));
    assert!(code.contains("const NAME: &'static str = \"api.WireName\""));
    assert!(code.contains("pub trait WireName"));
    assert!(code.contains("fn rust_method("));
    assert!(code.contains("PayloadShape::StreamingRawStruct"));
    assert!(code.contains("PayloadShape::UnaryArgsResult"));
    assert!(!code.contains("::volo_thrift"));
    assert!(crate::qualified_prelude(&code));
}

fn qualified_prelude(code: &str) -> bool {
    // Generated methods live beside user-defined IDL types and must not rely
    // on the user's Result/Option/Default bindings.
    !code.contains("=> Ok(") && !code.contains("=> Err(")
}

#[test]
fn config_builder_selects_the_whole_thrift_entry_and_preserves_old_default() {
    let root = tempfile::tempdir().unwrap();
    let idl = root.path().join("data.thrift");
    fs::write(
        &idl,
        "struct Message {} service Echo { Message Call(1: Message request) }",
    )
    .unwrap();
    for (config, grpc) in [
        ("", false),
        (
            "        codegen_option:\n          config: legacy-plugin-value\n",
            false,
        ),
        (
            "        codegen_option:\n          config:\n            wire_package: 123\n",
            false,
        ),
        (
            "        codegen_option:\n          config:\n            rpc_transport: grpc\n",
            true,
        ),
    ] {
        let path = root.path().join("volo.yml");
        fs::write(
            &path,
            format!(
                "entries:\n  default:\n    filename: generated.rs\n    protocol: thrift\n    \
                 services:\n      - idl:\n          source: local\n          path: {}\n{config}",
                idl.display()
            ),
        )
        .unwrap();
        ConfigBuilder::new(path)
            .out_dir(root.path())
            .write()
            .unwrap();
        let code = fs::read_to_string(root.path().join("generated.rs")).unwrap();
        assert_eq!(code.contains("::volo_grpc::"), grpc);
        assert_eq!(code.contains("::volo_thrift::"), !grpc);
    }
}

#[test]
fn explicit_facade_and_yaml_overrides_apply_to_the_entire_entry() {
    let root = tempfile::tempdir().unwrap();
    let idl = root.path().join("fixture.thrift");
    fs::write(
        &idl,
        "struct Message {} service Echo { Message Call(1: Message request) }",
    )
    .unwrap();
    Builder::thrift_grpc()
        .add_service(&idl)
        .service_config(
            &idl,
            serde_yaml::from_str("wire_package: yaml_package\nruntime_crate: '::yaml_runtime'")
                .unwrap(),
        )
        .thrift_grpc_options(volo_build::thrift_grpc_backend::Options {
            runtime_crate: Some("::my_runtime".into()),
            ..Default::default()
        })
        .out_dir(root.path())
        .write()
        .unwrap();
    let code = fs::read_to_string(root.path().join("volo_gen.rs")).unwrap();
    assert!(code.contains("/yaml_package.Echo/Call"));
    assert!(code.contains("::my_runtime::Request"));
    assert!(!code.contains("::volo_grpc::"));
    assert!(!code.contains("::yaml_runtime::"));
}

#[test]
fn conflicting_transports_fail_instead_of_silently_splitting_a_client() {
    let root = tempfile::tempdir().unwrap();
    let idl = root.path().join("data.thrift");
    fs::write(&idl, "struct Message {}").unwrap();
    let path = root.path().join("volo.yml");
    fs::write(&path, format!("entries:\n  default:\n    filename: generated.rs\n    protocol: thrift\n    services:\n      - idl:\n          source: local\n          path: {0}\n        codegen_option:\n          config:\n            rpc_transport: grpc\n      - idl:\n          source: local\n          path: {0}\n        codegen_option:\n          config:\n            rpc_transport: ttheader\n", idl.display())).unwrap();
    let error = ConfigBuilder::new(path)
        .out_dir(root.path())
        .write()
        .unwrap_err();
    assert!(error.to_string().contains("conflicting rpc_transport"));
}

#[test]
fn explicit_thrift_builder_rejects_grpc_config_instead_of_using_the_wrong_transport() {
    let root = tempfile::tempdir().unwrap();
    let idl = root.path().join("fixture.thrift");
    fs::write(&idl, "service Echo { void Ping() }").unwrap();
    let error = Builder::thrift()
        .add_service(&idl)
        .service_config(&idl, serde_yaml::from_str("rpc_transport: grpc").unwrap())
        .out_dir(root.path())
        .write()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires Builder::thrift_grpc()")
    );
    assert!(!root.path().join("volo_gen.rs").exists());
}
