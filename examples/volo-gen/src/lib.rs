//! Positive doctest: the target file's types are generated and usable.
//!
//! ```
//! let _ = volo_gen::thrift_no_service_gen::no_service_target::NoServiceRecord::default();
//! ```
//!
//! Negative doctest: the file that did NOT enable `no_service` must not
//! contribute any types to the generated module tree.
//!
//! ```compile_fail
//! let _ = volo_gen::thrift_no_service_gen::no_service_ignored::IgnoredRecord::default();
//! ```

mod r#gen {
    include!(concat!(env!("OUT_DIR"), "/thrift_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/thrift_no_service_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/proto_gen.rs"));
}

pub use r#gen::*;
