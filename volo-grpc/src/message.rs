use bytes::Bytes;
use hyper::Body;

use crate::codec::decode::Kind;

pub trait SendEntryMessage {
    fn into_body(self) -> crate::BoxStream<'static, Result<Bytes, crate::Status>>;
}

pub trait RecvEntryMessage: Sized {
    fn from_body(method: Option<&str>, body: Body, kind: Kind) -> Result<Self, crate::Status>;
}
