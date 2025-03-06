use bytes::Bytes;
use http_body::Frame;

use crate::{
    body::BoxBody,
    codec::{compression::CompressionEncoding, decode::Kind},
};

pub trait SendEntryMessage {
    fn into_body(
        self,
        compression_config: Option<CompressionEncoding>,
    ) -> crate::BoxStream<'static, Result<Frame<Bytes>, crate::Status>>;
}

#[allow(clippy::result_large_err)]
pub trait RecvEntryMessage: Sized {
    fn from_body(
        method: Option<&str>,
        body: BoxBody,
        kind: Kind,
        compression_encoding: Option<CompressionEncoding>,
    ) -> Result<Self, crate::Status>;
}
