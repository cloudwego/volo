//! These codes are copied from `tonic/src/codec/compression.rs` and may be modified by us.

use std::io;

use bytes::{Buf, BufMut, BytesMut};
use flate2::read::{GzDecoder, GzEncoder};
pub use flate2::Compression;
use http::HeaderValue;

use super::BUFFER_SIZE;
use crate::Status;

pub const ENCODING_HEADER: &str = "grpc-encoding";
pub const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";
const DEFAULT_GZIP_LEVEL: Compression = Compression::new(6);

/// The compression encodings volo supports.
#[derive(Clone, Copy, Debug)]
pub enum CompressionEncoding {
    Identity,
    Gzip(Option<GzipConfig>),
}

impl PartialEq for CompressionEncoding {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Gzip(_), Self::Gzip(_)) | (Self::Identity, Self::Identity)
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GzipConfig {
    pub level: Compression,
}

impl Default for GzipConfig {
    fn default() -> Self {
        Self {
            level: DEFAULT_GZIP_LEVEL,
        }
    }
}

impl CompressionEncoding {
    pub fn into_header_value(self) -> HeaderValue {
        match self {
            CompressionEncoding::Gzip(_) => HeaderValue::from_static("gzip"),
            CompressionEncoding::Identity => HeaderValue::from_static("identity"),
        }
    }

    pub fn into_accept_encoding_header_value(self) -> Option<HeaderValue> {
        if self.is_gzip_enabled() {
            Some(HeaderValue::from_static("gzip,identity"))
        } else {
            None
        }
    }

    /// Get the value of `grpc-encoding` header. Returns an error if the encoding isn't supported.
    pub fn from_encoding_header(
        map: &http::HeaderMap,
        config: Option<CompressionEncoding>,
    ) -> Option<CompressionEncoding> {
        if let Some(config) = config {
            let header_value = map.get(ACCEPT_ENCODING_HEADER)?;
            let header_value_str = header_value.to_str().ok()?;

            header_value_str
                .trim()
                .split(',')
                .map(|s| s.trim())
                .find_map(|encoding: &str| {
                    match encoding {
                        s if s.starts_with("gzip") => {
                            // gzip-6 @https://grpc.github.io/grpc/core/md_doc_compression.html#autotoc_md59
                            // Not implemented temporarily to reduce unnecessary parsing overhead
                            // let x: Vec<&str> = s.split("-").collect();
                            Some(config)
                        }
                        _ => None,
                    }
                })
        } else {
            None
        }
    }

    /// Based on the `grpc-accept-encoding` header, pick an encoding to use.
    pub fn from_accept_encoding_header(
        map: &http::HeaderMap,
        config: Option<Self>,
    ) -> Result<Option<CompressionEncoding>, Status> {
        if let Some(config) = config {
            let header_value = if let Some(value) = map.get(ENCODING_HEADER) {
                value
            } else {
                return Ok(None);
            };

            let header_value_str = if let Ok(value) = header_value.to_str() {
                value
            } else {
                return Ok(None);
            };

            match header_value_str {
                "gzip" => {
                    if config.is_gzip_enabled() {
                        Ok(Some(CompressionEncoding::Gzip(Some(GzipConfig::default()))))
                    } else {
                        Ok(None)
                    }
                }

                "identity" => Ok(None),
                other => {
                    let status = Status::unimplemented(format!(
                        "Content is compressed with `{}` which isn't supported",
                        other
                    ));

                    Err(status)
                }
            }
        } else {
            Ok(None)
        }
    }

    /// please use it only for Compression type is insignificant, otherwise you will have a
    /// duplicate pattern-matching problem
    pub fn level(self) -> Compression {
        match self {
            CompressionEncoding::Gzip(config) if let Some(config)=config =>{
                config.level
            } ,
            CompressionEncoding::Identity | _ => DEFAULT_GZIP_LEVEL,
        }
    }

    const fn is_gzip_enabled(&self) -> bool {
        if let CompressionEncoding::Gzip(_) = self {
            return true;
        }
        false
    }
}

/// Compress `len` bytes from `src_buf` into `dest_buf`.
pub(crate) fn compress(
    encoding: CompressionEncoding,
    src_buf: &mut BytesMut,
    dest_buf: &mut BytesMut,
) -> Result<(), io::Error> {
    let len = src_buf.len();
    let capacity = ((len / BUFFER_SIZE) + 1) * BUFFER_SIZE;

    dest_buf.reserve(capacity);
    match encoding {
        CompressionEncoding::Gzip(config) if let Some(config) = config  => {
            let mut gzip_encoder = GzEncoder::new(&src_buf[0..len], config.level);
            io::copy(&mut gzip_encoder, &mut dest_buf.writer())?;
        }
        _ => {}
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

    match encoding {
        CompressionEncoding::Gzip(_) => {
            let mut gzip_decoder = GzDecoder::new(&src_buf[0..len]);
            io::copy(&mut gzip_decoder, &mut dest_buf.writer())?;
        }
        _ => {}
    }

    src_buf.advance(len);
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};

    use crate::codec::{
        compression::{compress, decompress, Compression, CompressionEncoding, GzipConfig},
        BUFFER_SIZE,
    };

    #[test]
    fn test_consistency_for_gzip() {
        let mut src = BytesMut::with_capacity(BUFFER_SIZE);
        let mut compress_buf = BytesMut::new();
        let test_data = &b"test compression"[..];

        src.put(test_data);

        compress(
            CompressionEncoding::Gzip(Some(GzipConfig {
                level: Compression::fast(),
            })),
            &mut src,
            &mut compress_buf,
        )
        .expect("compress failed:");

        let mut decompressed_data = BytesMut::with_capacity(BUFFER_SIZE);

        decompress(
            CompressionEncoding::Gzip(None),
            &mut compress_buf,
            &mut decompressed_data,
        )
        .expect("decompress failed:");

        assert_eq!(test_data, decompressed_data);
    }
}
