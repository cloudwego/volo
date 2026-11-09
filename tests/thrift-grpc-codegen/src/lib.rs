include!(concat!(env!("OUT_DIR"), "/volo_gen.rs"));

include!(concat!(env!("OUT_DIR"), "/ttstream.rs"));
include!(concat!(env!("OUT_DIR"), "/shared.rs"));

pub mod fixture;
pub mod metadata_values;
pub mod phase_latency;
