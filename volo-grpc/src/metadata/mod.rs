//! Contains data structures and utilities for handling gRPC custom metadata and may be modified by
//! us.

mod encoding;
mod key;
mod map;
mod value;

pub(crate) use self::map::GRPC_TIMEOUT_HEADER;
pub use self::{
    encoding::{Ascii, Binary},
    key::{AsciiMetadataKey, BinaryMetadataKey, MetadataKey},
    map::{
        Entry, GetAll, Iter, IterMut, KeyAndMutValueRef, KeyAndValueRef, KeyRef, Keys, MetadataMap,
        OccupiedEntry, VacantEntry, ValueDrain, ValueIter, ValueRef, ValueRefMut, Values,
        ValuesMut,
    },
    value::{AsciiMetadataValue, BinaryMetadataValue, MetadataValue},
};
pub mod errors {
    pub use super::{
        encoding::{InvalidMetadataValue, InvalidMetadataValueBytes},
        key::InvalidMetadataKey,
        value::ToStrError,
    };
}

pub const DESTINATION_SERVICE: &str = "destination-service";
pub const DESTINATION_METHOD: &str = "destination-method";
pub const DESTINATION_ADDR: &str = "destination-addr";

pub const SOURCE_SERVICE: &str = "source-service";

pub const HEADER_TRANS_REMOTE_ADDR: &str = "rip";
