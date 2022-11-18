//! These codes are copied from `tonic/src/codec/compression.rs` and may be modified by us.

use std::io;

use bytes::{Buf, BufMut, BytesMut};
use flate2::{
    read::{GzDecoder, GzEncoder},
    Compression,
};
use http::HeaderValue;

use super::BUFFER_SIZE;

pub const ENCODING_HEADER: &str = "grpc-encoding";
pub const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";

/// Struct used to configure which encodings on a server or channel.
#[derive(Debug, Clone, Copy)]
pub struct CompressionConfig {
    pub encoding: CompressionEncoding,
    pub level: u32,
}

impl CompressionConfig {
    pub(crate) fn into_accept_encoding_header_value(self) -> Option<HeaderValue> {
        if self.is_gzip_enabled() {
            Some(HeaderValue::from_static("gzip,identity"))
        } else {
            None
        }
    }

    const fn is_gzip_enabled(&self) -> bool {
        if let CompressionEncoding::Gzip = self.encoding {
            return true;
        }
        false
    }
}

/// The compression encodings volo supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressionEncoding {
    // or Gzip(Level) ?
    Gzip,
    None,
}

impl CompressionEncoding {
    // /// Based on the `grpc-accept-encoding` header, pick an encoding to use.
    // pub(crate) fn from_accept_encoding_header(map: &http::HeaderMap) -> Option<Self> {
    //     let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
    //     let header_value_str = header_value.to_str().ok()?;
    //
    //     header_value_str
    //         .trim()
    //         .split(',')
    //         .map(|s| s.trim())
    //         .find_map(|value| match value {
    //             "gzip" => Some(CompressionEncoding::Gzip),
    //             _ => None,
    //         })
    // }
    //
    // /// Get the value of `grpc-encoding` header. Returns an error if the encoding isn't
    // supported. pub(crate) fn from_encoding_header(map: &http::HeaderMap) ->
    // Result<Option<Self>, Status> {     let header_value = if let Some(value) =
    // map.get(ENCODING_HEADER) {         value
    //     } else {
    //         return Ok(None);
    //     };
    //
    //     let header_value_str = if let Ok(value) = header_value.to_str() {
    //         value
    //     } else {
    //         return Ok(None);
    //     };
    //
    //     match header_value_str {
    //         "gzip" => Ok(Some(CompressionEncoding::Gzip)),
    //
    //         "identity" => Ok(None),
    //         other => {
    //             let status = Status::unimplemented(format!(
    //                 "Content is compressed with `{}` which isn't supported",
    //                 other
    //             ));
    //
    //             Err(status)
    //         }
    //     }
    // }

    pub fn into_header_value(self) -> HeaderValue {
        match self {
            CompressionEncoding::Gzip => HeaderValue::from_static("gzip"),
            CompressionEncoding::None => HeaderValue::from_static(""),
        }
    }
}

/// Compress `len` bytes from `src_buf` into `dest_buf`.
pub(crate) fn compress(
    encoding: CompressionEncoding,
    src_buf: &mut BytesMut,
    dest_buf: &mut BytesMut,
    level: Compression,
) -> Result<(), io::Error> {
    let len = src_buf.len();
    let capacity = ((len / BUFFER_SIZE) + 1) * BUFFER_SIZE;

    dest_buf.reserve(capacity);

    if encoding == CompressionEncoding::Gzip {
        let mut gzip_encoder = GzEncoder::new(&src_buf[0..len], level);
        io::copy(&mut gzip_encoder, &mut dest_buf.writer())?;
    }

    src_buf.advance(len);
    Ok(())
}

/// Decompress `len` bytes from `src_buf` into `dest_buf`.
pub(crate) fn decompress(
    encoding: CompressionEncoding,
    src_buf: &mut BytesMut,
    dest_buf: &mut BytesMut,
) -> Result<(), io::Error> {
    let len = src_buf.len();
    let estimate_decompressed_len = len * 2;
    let capacity = ((estimate_decompressed_len / BUFFER_SIZE) + 1) * BUFFER_SIZE;

    dest_buf.reserve(capacity);

    if encoding == CompressionEncoding::Gzip {
        let mut gzip_decoder = GzDecoder::new(&src_buf[0..len]);
        io::copy(&mut gzip_decoder, &mut dest_buf.writer())?;
    }

    src_buf.advance(len);
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};
    use flate2::Compression;

    use crate::codec::{
        compression::{compress, decompress, CompressionEncoding},
        BUFFER_SIZE,
    };

    #[test]
    fn test_consistency() {
        let mut src = BytesMut::with_capacity(BUFFER_SIZE);
        let mut compress_buf = BytesMut::new();
        let test_data = &b"test compression"[..];

        src.put(test_data);

        compress(
            CompressionEncoding::Gzip,
            &mut src,
            &mut compress_buf,
            Compression::fast(),
        )
        .expect("compress failed:");

        let mut decompressed_data = BytesMut::with_capacity(BUFFER_SIZE);

        decompress(
            CompressionEncoding::Gzip,
            &mut compress_buf,
            &mut decompressed_data,
        )
        .expect("decompress failed:");

        assert_eq!(test_data, decompressed_data);
    }
}
