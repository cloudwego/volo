pub mod perf;
pub mod runner;

mod gen {
    include!(concat!(env!("OUT_DIR"), "/benchmark.rs"));
}

pub use gen::*;
