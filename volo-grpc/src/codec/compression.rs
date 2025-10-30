//! These codes are copied from `tonic/src/codec/compression.rs` and may be modified by us.

use std::io;

#[cfg(feature = "compress")]
use bytes::BufMut;
use bytes::{Buf, BytesMut};
#[cfg(feature = "compress")]
pub use flate2::Compression as Level;
#[cfg(feature = "gzip")]
use flate2::bufread::{GzDecoder, GzEncoder};
#[cfg(feature = "zlib")]
use flate2::bufread::{ZlibDecoder, ZlibEncoder};
use http::HeaderValue;
use pilota::LinkedBytes;

use super::BUFFER_SIZE;
#[cfg(feature = "compress")]
use crate::Status;

pub const ENCODING_HEADER: &str = "grpc-encoding";
pub const ACCEPT_ENCODING_HEADER: &str = "grpc-accept-encoding";
#[cfg(feature = "compress")]
const DEFAULT_LEVEL: Level = Level::new(6);

/// The compression encodings volo supports.
#[derive(Clone, Copy, Debug)]
pub enum CompressionEncoding {
    Identity,
    #[cfg(feature = "gzip")]
    Gzip(Option<GzipConfig>),
    #[cfg(feature = "zlib")]
    Zlib(Option<ZlibConfig>),
    #[cfg(feature = "zstd")]
    Zstd(Option<ZstdConfig>),
}

impl PartialEq for CompressionEncoding {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            #[cfg(feature = "gzip")]
            (Self::Gzip(_), Self::Gzip(_)) => true,
            #[cfg(feature = "zlib")]
            (Self::Zlib(_), Self::Zlib(_)) => true,
            (Self::Identity, Self::Identity) => true,
            #[cfg(feature = "zstd")]
            (Self::Zstd(_), Self::Zstd(_)) => true,
            #[cfg(feature = "compress")]
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg(feature = "gzip")]
pub struct GzipConfig {
    pub level: Level,
}

#[cfg(feature = "gzip")]
impl Default for GzipConfig {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg(feature = "zlib")]
pub struct ZlibConfig {
    pub level: Level,
}

#[cfg(feature = "zlib")]
impl Default for ZlibConfig {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg(feature = "zstd")]
pub struct ZstdConfig {
    pub level: Level,
}

#[cfg(feature = "zstd")]
impl Default for ZstdConfig {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
        }
    }
}

/// compose multiple compression encodings to a [HeaderValue]
pub fn compose_encodings(encodings: &[CompressionEncoding]) -> HeaderValue {
    let encodings = encodings
        .iter()
        .map(|item| match item {
            // TODO: gzip-6 @https://grpc.github.io/grpc/core/md_doc_compression.html#autotoc_md59
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip(_) => "gzip",
            #[cfg(feature = "zlib")]
            CompressionEncoding::Zlib(_) => "zlib",
            #[cfg(feature = "zstd")]
            CompressionEncoding::Zstd(_) => "zstd",
            CompressionEncoding::Identity => "identity",
        })
        .collect::<Vec<&'static str>>();
    // encodings.push("identity");

    HeaderValue::from_str(encodings.join(",").as_str()).unwrap()
}

#[cfg(feature = "compress")]
fn is_enabled(encoding: CompressionEncoding, encodings: &[CompressionEncoding]) -> bool {
    encodings.contains(&encoding)
}

impl CompressionEncoding {
    /// make the compression encoding into a [HeaderValue]
    pub fn into_header_value(self) -> HeaderValue {
        match self {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip(_) => HeaderValue::from_static("gzip"),
            #[cfg(feature = "zlib")]
            CompressionEncoding::Zlib(_) => HeaderValue::from_static("zlib"),
            #[cfg(feature = "zstd")]
            CompressionEncoding::Zstd(_) => HeaderValue::from_static("zstd"),
            CompressionEncoding::Identity => HeaderValue::from_static("identity"),
        }
    }

    /// make the compression encodings into a [HeaderValue],and the encodings uses a `,` as
    /// separator
    pub fn into_accept_encoding_header_value(
        self,
        encodings: &[CompressionEncoding],
    ) -> Option<HeaderValue> {
        if self.is_enabled() {
            Some(compose_encodings(encodings))
        } else {
            None
        }
    }

    /// Based on the `grpc-accept-encoding` header, adaptive picking an encoding to use.
    #[cfg(feature = "compress")]
    pub fn from_accept_encoding_header(
        headers: &http::HeaderMap,
        config: &Option<Vec<Self>>,
    ) -> Option<Self> {
        if let Some(available_encodings) = config {
            let header_value = headers.get(ACCEPT_ENCODING_HEADER)?;
            let header_value_str = header_value.to_str().ok()?;

            header_value_str
                .split(',')
                .map(|s| s.trim())
                .find_map(|encoding| match encoding {
                    #[cfg(feature = "gzip")]
                    "gzip" => available_encodings.iter().find_map(|item| {
                        if item.is_gzip_enabled() {
                            Some(*item)
                        } else {
                            None
                        }
                    }),
                    #[cfg(feature = "zlib")]
                    "zlib" => available_encodings.iter().find_map(|item| {
                        if item.is_zlib_enabled() {
                            Some(*item)
                        } else {
                            None
                        }
                    }),
                    #[cfg(feature = "zstd")]
                    "zstd" => available_encodings.iter().find_map(|item| {
                        if item.is_zstd_enabled() {
                            Some(*item)
                        } else {
                            None
                        }
                    }),
                    _ => None,
                })
        } else {
            None
        }
    }

    /// Get the value of `grpc-encoding` header. Returns an error if the encoding isn't supported.
    #[allow(clippy::result_large_err)]
    #[cfg(feature = "compress")]
    pub fn from_encoding_header(
        headers: &http::HeaderMap,
        config: &Option<Vec<Self>>,
    ) -> Result<Option<Self>, Status> {
        if let Some(encodings) = config {
            let header_value = if let Some(header_value) = headers.get(ENCODING_HEADER) {
                header_value
            } else {
                return Ok(None);
            };

            match header_value.to_str()? {
                #[cfg(feature = "gzip")]
                "gzip" if is_enabled(Self::Gzip(None), encodings) => Ok(Some(Self::Gzip(None))),
                #[cfg(feature = "zlib")]
                "zlib" if is_enabled(Self::Zlib(None), encodings) => Ok(Some(Self::Zlib(None))),
                #[cfg(feature = "zstd")]
                "zstd" if is_enabled(Self::Zstd(None), encodings) => Ok(Some(Self::Zstd(None))),
                "identity" => Ok(None),
                other => {
                    let status = Status::unimplemented(format!(
                        "Content is compressed with `{other}` which isn't supported"
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
    #[cfg(feature = "compress")]
    pub fn level(self) -> Level {
        match self {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip(Some(config)) => config.level,
            #[cfg(feature = "zlib")]
            CompressionEncoding::Zlib(Some(config)) => config.level,
            #[cfg(feature = "zstd")]
            CompressionEncoding::Zstd(Some(config)) => config.level,
            _ => DEFAULT_LEVEL,
        }
    }

    #[cfg(feature = "gzip")]
    const fn is_gzip_enabled(&self) -> bool {
        matches!(self, CompressionEncoding::Gzip(_))
    }

    #[cfg(feature = "zlib")]
    const fn is_zlib_enabled(&self) -> bool {
        matches!(self, CompressionEncoding::Zlib(_))
    }

    #[cfg(feature = "zstd")]
    const fn is_zstd_enabled(&self) -> bool {
        matches!(self, CompressionEncoding::Zstd(_))
    }

    const fn is_enabled(&self) -> bool {
        #[allow(unreachable_patterns)]
        match self {
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip(_) => true,
            #[cfg(feature = "zlib")]
            CompressionEncoding::Zlib(_) => true,
            #[cfg(feature = "zstd")]
            CompressionEncoding::Zstd(_) => true,
            _ => false,
        }
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
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip(Some(config)) => {
            let mut gz_encoder = GzEncoder::new(&src_buf[0..len], config.level);
            io::copy(&mut gz_encoder, &mut dest_buf.writer())?;
        }
        #[cfg(feature = "zlib")]
        CompressionEncoding::Zlib(Some(config)) => {
            let mut zlib_encoder = ZlibEncoder::new(&src_buf[0..len], config.level);
            io::copy(&mut zlib_encoder, &mut dest_buf.writer())?;
        }
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd(Some(config)) => {
            let level = config.level.level();
            let zstd_level = if level == 0 {
                zstd::DEFAULT_COMPRESSION_LEVEL
            } else {
                level as i32
            };
            let mut zstd_encoder = zstd::Encoder::new(dest_buf.writer(), zstd_level)?;
            io::copy(&mut &src_buf[0..len], &mut zstd_encoder)?;
            zstd_encoder.finish()?;
        }
        _ => {}
    };

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
        #[cfg(feature = "gzip")]
        CompressionEncoding::Gzip(_) => {
            let mut gz_decoder = GzDecoder::new(&src_buf[0..len]);
            io::copy(&mut gz_decoder, &mut dest_buf.writer())?;
        }
        #[cfg(feature = "zlib")]
        CompressionEncoding::Zlib(_) => {
            let mut zlib_decoder = ZlibDecoder::new(&src_buf[0..len]);
            io::copy(&mut zlib_decoder, &mut dest_buf.writer())?;
        }
        #[cfg(feature = "zstd")]
        CompressionEncoding::Zstd(_) => {
            let mut zstd_decoder = zstd::Decoder::new(&src_buf[0..len])?;
            io::copy(&mut zstd_decoder, &mut dest_buf.writer())?;
        }
        _ => {}
    };

    src_buf.advance(len);
    Ok(())
}

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};
    use pilota::LinkedBytes;

    #[cfg(feature = "gzip")]
    use crate::codec::compression::GzipConfig;
    #[cfg(feature = "compress")]
    use crate::codec::compression::Level;
    #[cfg(feature = "zlib")]
    use crate::codec::compression::ZlibConfig;
    #[cfg(feature = "zstd")]
    use crate::codec::compression::ZstdConfig;
    use crate::codec::{
        BUFFER_SIZE,
        compression::{CompressionEncoding, compress, decompress},
    };

    #[test]
    fn test_consistency_for_compression() {
        let mut src = BytesMut::with_capacity(BUFFER_SIZE);
        let mut compress_buf = BytesMut::new();
        let mut de_data = BytesMut::with_capacity(BUFFER_SIZE);
        let test_data = &b"test compression"[..];
        src.extend_from_slice(test_data);

        let encodings = [
            #[cfg(feature = "gzip")]
            CompressionEncoding::Gzip(Some(GzipConfig {
                level: Level::fast(),
            })),
            #[cfg(feature = "zlib")]
            CompressionEncoding::Zlib(Some(ZlibConfig {
                level: Level::fast(),
            })),
            #[cfg(feature = "zstd")]
            CompressionEncoding::Zstd(Some(ZstdConfig {
                level: Level::new(3),
            })),
            CompressionEncoding::Identity,
        ];

        for encoding in encodings {
            compress_buf.clear();
            compress(encoding, &mut src, &mut compress_buf).expect("compress failed:");
            decompress(encoding, &mut compress_buf, &mut de_data).expect("decompress failed:");
            assert_eq!(test_data, de_data.as_ref());
        }
    }
}
