include!(concat!(env!("OUT_DIR"), "/thrift/generated.rs"));
pub mod thrift {
    pub use super::generated::*;
}
pub mod protobuf {
    include!(concat!(env!("OUT_DIR"), "/protobuf/generated.rs"));
    pub use generated::*;
}

pub fn inherited_request(value: thrift::boundary_parent::Value) -> thrift::boundary_parent::Value {
    value
}
