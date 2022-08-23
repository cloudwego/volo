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
