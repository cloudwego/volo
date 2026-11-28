include!(concat!(env!("OUT_DIR"), "/arc_shapes_thrift.rs"));
include!(concat!(env!("OUT_DIR"), "/arc_shapes_grpc.rs"));
include!(concat!(env!("OUT_DIR"), "/thrift.rs"));
include!(concat!(env!("OUT_DIR"), "/protobuf.rs"));
include!(concat!(env!("OUT_DIR"), "/grpc.rs"));
include!(concat!(env!("OUT_DIR"), "/split_thrift.rs"));
include!(concat!(env!("OUT_DIR"), "/split_protobuf.rs"));
include!(concat!(env!("OUT_DIR"), "/names_pb.rs"));
include!(concat!(env!("OUT_DIR"), "/names_thrift.rs"));
include!(concat!(env!("OUT_DIR"), "/names_override.rs"));
include!(concat!(env!("OUT_DIR"), "/name_edges_thrift.rs"));
include!(concat!(env!("OUT_DIR"), "/name_edges_grpc.rs"));
include!(concat!(env!("OUT_DIR"), "/name_edges_pb.rs"));
include!(concat!(env!("OUT_DIR"), "/oneway.rs"));

pub mod perf_server;
