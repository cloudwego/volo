mod gen {
    volo::include_service!("thrift_gen.rs");
    volo::include_service!("proto_gen.rs");
}

pub use gen::*;
