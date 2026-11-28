use std::fs;

use volo_build::Builder;
use volo_streamx_build::{Options, StreamXPlugin, plugin};

#[test]
fn duplicate_plugins_and_invalid_paths_fail_before_any_file_is_emitted() {
    for invalid_path in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("fixture.thrift");
        fs::write(
            &input,
            "struct Message {} service Echo { Message Call(1: Message request) }",
        )
        .unwrap();
        let second = if invalid_path {
            StreamXPlugin::with_options(Options {
                runtime_crate: "bad + path".into(),
            })
        } else {
            plugin()
        };
        let error = Builder::thrift()
            .add_service(&input)
            .plugin(plugin())
            .plugin(second)
            .out_dir(root.path())
            .write()
            .unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("Echo"), "{diagnostic}");
        if !invalid_path {
            assert!(
                diagnostic.contains("duplicate generated item `echo_streamx`"),
                "{diagnostic}"
            );
        }
        assert!(!root.path().join("volo_gen.rs").exists());
        Builder::thrift()
            .add_service(&input)
            .plugin(plugin())
            .out_dir(root.path())
            .write()
            .unwrap();
        assert!(root.path().join("volo_gen.rs").exists());
    }
}

#[test]
fn explicit_backend_wire_options_reach_the_attached_binding_in_either_plugin_order() {
    for plugin_first in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("fixture.thrift");
        fs::write(
            &input,
            "struct Message {} service Echo { Message Call(1: Message request) }",
        )
        .unwrap();
        let options = volo_build::thrift_grpc_backend::Options {
            wire_services: [("Echo".into(), "custom.Endpoint".into())].into(),
            ..Default::default()
        };
        let builder = Builder::thrift_grpc().add_service(input);
        let builder = if plugin_first {
            builder.plugin(plugin()).thrift_grpc_options(options)
        } else {
            builder.thrift_grpc_options(options).plugin(plugin())
        };
        builder.out_dir(root.path()).write().unwrap();
        let source = fs::read_to_string(root.path().join("volo_gen.rs")).unwrap();
        assert!(
            source.contains("wire_name: \"custom.Endpoint\""),
            "{source}"
        );
        assert!(!source.contains("/fixture.Echo/Call"));
    }
}
