use bytes::Bytes;
use hyper::Body;

use crate::codec::{compression::CompressionEncoding, decode::Kind};

pub trait SendEntryMessage {
    fn into_body(
        self,
        compression_config: Option<CompressionEncoding>,
    ) -> crate::BoxStream<'static, Result<Bytes, crate::Status>>;
}

pub trait RecvEntryMessage: Sized {
    fn from_body(
        method: Option<&str>,
        body: Body,
        kind: Kind,
        compression_encoding: Option<CompressionEncoding>,
    ) -> Result<Self, crate::Status>;
}
