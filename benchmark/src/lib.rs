pub mod perf;
pub mod runner;

mod r#gen {
    include!(concat!(env!("OUT_DIR"), "/benchmark.rs"));
}

pub use r#gen::*;
