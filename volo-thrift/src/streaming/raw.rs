use bytes::Bytes;
use pilota::thrift::{TInputProtocol, TType, binary::TBinaryProtocol};

use super::StreamError;

/// A binary Thrift struct payload, distinct from a unary message envelope.
///
/// Validation consumes one complete struct with Pilota's bounded skip depth.
/// Like Kitex FastRead, a suffix after its STOP byte is accepted and preserved.
/// Constructing this type validates a struct prefix, not a method's IDL schema.
/// A zero-prefixed unversioned envelope can also parse as an empty struct plus
/// suffix. This type distinguishes API intent; callers still supply raw struct
/// bytes according to the method descriptor, rather than a message envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawStruct(Bytes);
impl RawStruct {
    pub fn from_bytes(bytes: Bytes) -> Result<Self, StreamError> {
        let mut remaining = bytes.clone();
        TBinaryProtocol::new(&mut remaining, false)
            .skip(TType::Struct)
            .map_err(|error| StreamError::Protocol(error.to_string()))?;
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &Bytes {
        &self.0
    }
    pub fn into_bytes(self) -> Bytes {
        self.0
    }
}
impl TryFrom<Bytes> for RawStruct {
    type Error = StreamError;
    fn try_from(bytes: Bytes) -> Result<Self, Self::Error> {
        Self::from_bytes(bytes)
    }
}
impl AsRef<[u8]> for RawStruct {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn struct_validation_preserves_suffix_and_rejects_versioned_envelope_or_truncation() {
        let bytes = Bytes::from_static(&[0, 0xa1, 0xb2]);
        let pointer = bytes.as_ptr();
        let raw = RawStruct::try_from(bytes).unwrap();
        assert_eq!(raw.as_bytes().as_ptr(), pointer);
        assert_eq!(raw.into_bytes().as_ref(), &[0, 0xa1, 0xb2]);
        for invalid in [
            Bytes::new(),
            Bytes::from_static(&[8, 0, 1, 0]),
            Bytes::from_static(&[0x80, 1, 0, 1, 0, 0, 0, 0]),
        ] {
            assert!(RawStruct::from_bytes(invalid).is_err());
        }
        let mut deep = Vec::new();
        for _ in 0..128 {
            deep.extend_from_slice(&[12, 0, 1]);
        }
        deep.extend(std::iter::repeat_n(0, 129));
        assert!(RawStruct::from_bytes(deep.into()).is_err());
    }
}
