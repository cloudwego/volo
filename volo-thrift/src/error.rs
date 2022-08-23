pub use pilota::thrift::error::*;

pub type Result<T, E = Error> = core::result::Result<T, E>;
