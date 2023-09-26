//! Generic encoding and decoding.
//!
//! This module contains the generic `Encoder` and `Decoder` traits as well as
//! the 'DefaultEncoder' and 'DefaultDecoder' implementations based on prost.

pub mod compression;
pub mod decode;
pub mod encode;

use std::{io, marker::PhantomData, mem::size_of};

use bytes::BytesMut;
use pilota::prost::Message;

use crate::{status::Code::Internal, Status};

const PREFIX_LEN: usize = size_of::<u32>() + size_of::<u8>();
const BUFFER_SIZE: usize = 8 * 1024;

/// Encoder for gRPC messages.
pub trait Encoder {
    /// The type that is encoded.
    type Item;

    /// The type of encoding errors.
    ///
    /// The type of unrecoverable frame encoding errors.
    type Error: From<io::Error>;

    /// Encodes a message into the buffer.
    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone)]
pub struct DefaultEncoder<T>(PhantomData<T>);

impl<T: Message> Encoder for DefaultEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
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
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error>;
}

#[derive(Debug, Clone)]
pub struct DefaultDecoder<T>(PhantomData<fn(T)>);

impl<T: Message + Default> Decoder for DefaultDecoder<T> {
    type Item = T;
    type Error = Status;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
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
