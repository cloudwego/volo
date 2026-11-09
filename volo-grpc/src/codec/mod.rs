//! Generic encoding and decoding.
//!
//! This module contains the generic `Encoder` and `Decoder` traits as well as
//! the 'DefaultEncoder' and 'DefaultDecoder' implementations based on prost.

pub mod compression;
pub mod decode;
pub mod encode;
#[cfg(feature = "thrift")]
pub mod thrift;

use std::{io, marker::PhantomData, mem::size_of};

use bytes::Bytes;
use pilota::{LinkedBytes, pb::Message};

use crate::{Status, status::Code::Internal};

const PREFIX_LEN: usize = size_of::<u32>() + size_of::<u8>();
const BUFFER_SIZE: usize = 8 * 1024;

/// Per-message wire and decoded size budgets, independent of HTTP/2 windows.
#[derive(Clone, Copy, Debug)]
pub struct CodecLimits {
    pub max_send_message_size: usize,
    pub max_receive_message_size: usize,
    pub max_decompressed_message_size: usize,
}

impl CodecLimits {
    /// Preserve the size range of the original Protobuf entry points.
    pub const fn legacy() -> Self {
        Self {
            max_send_message_size: u32::MAX as usize,
            max_receive_message_size: u32::MAX as usize,
            max_decompressed_message_size: usize::MAX,
        }
    }
}

impl Default for CodecLimits {
    fn default() -> Self {
        const MAX: usize = 16 * 1024 * 1024;
        Self {
            max_send_message_size: MAX,
            max_receive_message_size: MAX,
            max_decompressed_message_size: MAX,
        }
    }
}

/// Encoder for gRPC messages.
pub trait Encoder {
    /// Encode one payload subject to a size budget. Codecs that know its size
    /// can reject before reserving memory; other encoders retain their old API.
    fn encode_with_limit(
        &mut self,
        item: Self::Item,
        dst: &mut LinkedBytes,
        max_size: usize,
    ) -> Result<(), Status>
    where
        Self::Error: Into<Status>,
    {
        let start = dst.len();
        self.encode(item, dst).map_err(Into::into)?;
        if dst.len().saturating_sub(start) > max_size {
            return Err(Status::resource_exhausted(
                "encoded message exceeds size limit",
            ));
        }
        Ok(())
    }
    /// The type that is encoded.
    type Item;

    /// The type of encoding errors.
    ///
    /// The type of unrecoverable frame encoding errors.
    type Error: From<io::Error>;

    /// Encodes a message into the buffer.
    fn encode(&mut self, item: Self::Item, dst: &mut LinkedBytes) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone)]
pub struct DefaultEncoder<T>(PhantomData<T>);

impl<T: Message> Encoder for DefaultEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut LinkedBytes) -> Result<(), Self::Error> {
        let mut ctx = pilota::pb::EncodeLengthContext::default();
        let required_len = item.encoded_len(&mut ctx) - ctx.zero_copy_len;
        dst.reserve(required_len);
        item.encode(dst)
            .map_err(|e| Status::new(Internal, e.to_string()))
    }
}

impl<T> Default for DefaultEncoder<T> {
    fn default() -> Self {
        DefaultEncoder(PhantomData)
    }
}

/// Decoder for gRPC messages.
pub trait Decoder {
    /// The type that is decoded.
    type Item;

    /// The type of unrecoverable frame decoding errors.
    type Error: From<io::Error>;

    /// Decode a message from the buffer.
    fn decode(&mut self, src: Bytes) -> Result<Option<Self::Item>, Self::Error>;
}

#[derive(Debug, Clone)]
pub struct DefaultDecoder<T>(PhantomData<fn(T)>);

impl<T: Message + Default> Decoder for DefaultDecoder<T> {
    type Item = T;
    type Error = Status;

    fn decode(&mut self, src: Bytes) -> Result<Option<Self::Item>, Self::Error> {
        Message::decode(src)
            .map(Some)
            .map_err(|e| Status::new(Internal, e.to_string()))
    }
}

impl<T> Default for DefaultDecoder<T> {
    fn default() -> Self {
        DefaultDecoder(PhantomData)
    }
}
