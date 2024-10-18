mod gen {
    include!(concat!(env!("OUT_DIR"), "/thrift_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/proto_gen.rs"));
}

pub use gen::*;
