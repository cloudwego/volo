//! Thrift Binary struct payloads inside the standard gRPC envelope.
//!
//! Unary methods use generated Args/Result structs; streaming methods use the
//! raw IDL message. There is no Thrift message header in either case.

mod checked;

use std::marker::PhantomData;

use bytes::Bytes;
use pilota::{
    LinkedBytes,
    thrift::{
        Message, ProtocolExceptionKind, TLengthProtocol, ThriftException, binary::TBinaryProtocol,
    },
};

use super::{Decoder, Encoder, decode::RecvStream};
use crate::Status;

pub type ThriftRecvStream<T> = RecvStream<T, ThriftDecoder<T>>;

#[derive(Debug)]
pub struct ThriftEncoder<T>(PhantomData<fn(T)>);

impl<T> Default for ThriftEncoder<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T: Message> Encoder for ThriftEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode_with_limit(
        &mut self,
        item: T,
        dst: &mut LinkedBytes,
        max_size: usize,
    ) -> Result<(), Status> {
        let mut sizing = TBinaryProtocol::new((), true);
        let size = item.size(&mut sizing);
        if size > max_size {
            return Err(Status::resource_exhausted(
                "encoded message exceeds size limit",
            ));
        }
        dst.reserve(size.saturating_sub(sizing.zero_copy_len()));
        item.encode(&mut TBinaryProtocol::new(dst, true))
            .map_err(|error| Status::internal(format!("Thrift encoding: {error}")))
    }

    fn encode(&mut self, item: T, dst: &mut LinkedBytes) -> Result<(), Status> {
        self.encode_with_limit(item, dst, usize::MAX)
    }
}

#[derive(Debug)]
pub struct ThriftDecoder<T> {
    _marker: PhantomData<fn(T)>,
    max_depth: usize,
    max_container_len: usize,
    context: Option<crate::streaming::StreamContext>,
    exception_classifier: Option<fn(&T) -> bool>,
}

impl<T> Default for ThriftDecoder<T> {
    fn default() -> Self {
        Self {
            _marker: PhantomData,
            max_depth: 64,
            max_container_len: 1 << 20,
            context: None,
            exception_classifier: None,
        }
    }
}

impl<T> ThriftDecoder<T> {
    pub fn with_exception_classifier(mut self, classify: fn(&T) -> bool) -> Self {
        self.exception_classifier = Some(classify);
        self
    }
    pub fn with_context(mut self, context: Option<crate::streaming::StreamContext>) -> Self {
        self.context = context;
        self
    }
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    pub fn with_max_container_len(mut self, max_container_len: usize) -> Self {
        self.max_container_len = max_container_len;
        self
    }
}

impl<T: Message> Decoder for ThriftDecoder<T> {
    type Item = T;
    type Error = Status;

    fn decode(&mut self, mut src: Bytes) -> Result<Option<T>, Status> {
        // Match Kitex FastRead: decode one struct, permitting a payload suffix.
        // The framing layer already bounds and owns the complete message.
        let result = T::decode(&mut checked::CheckedBinaryInput::new(&mut src, self.max_depth, self.max_container_len))
            .map(Some)
            .map_err(|error| {
                let message = format!("Thrift decoding: {error}");
                if matches!(&error, ThriftException::Protocol(error) if matches!(error.kind(), ProtocolExceptionKind::DepthLimit | ProtocolExceptionKind::SizeLimit)) {
                    Status::resource_exhausted(message)
                } else {
                    Status::internal(message)
                }
            });
        if let (Some(context), Some(classify), Ok(Some(value))) =
            (&self.context, self.exception_classifier, &result)
        {
            if classify(value) {
                context.record_declared_exception();
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use pilota::thrift::{ApplicationException, ApplicationExceptionKind, TInputProtocol};

    use super::*;
    use crate::Code;

    #[test]
    fn truncated_variable_lengths_never_panic_or_allocate_from_the_claimed_size() {
        for bytes in [
            b"\x0b\0\x01\xff\xff\xff\xff\0".as_slice(),
            b"\x0b\0\x01\0\0\0\x08x\0",
            b"\x0f\0\x63\x0a\xff\xff\xff\xff\0",
            b"\x0d\0\x63\x0b\x0a\xff\xff\xff\xff\0",
            b"\x0e\0\x63\x0a\xff\xff\xff\xff\0",
        ] {
            assert!(
                ThriftDecoder::<ApplicationException>::default()
                    .decode(Bytes::copy_from_slice(bytes))
                    .is_err()
            );
        }
        let bytes = Bytes::from_static(b"\x0f\0\x63\x0a\x7f\xff\xff\xff\0");
        assert_eq!(
            ThriftDecoder::<ApplicationException>::default()
                .decode(bytes.clone())
                .unwrap_err()
                .code(),
            Code::ResourceExhausted
        );
        assert_eq!(
            ThriftDecoder::<ApplicationException>::default()
                .with_max_container_len(usize::MAX)
                .decode(bytes)
                .unwrap_err()
                .code(),
            Code::Internal
        );
    }

    #[test]
    fn all_truncations_of_a_valid_struct_are_errors() {
        let value =
            ApplicationException::new(ApplicationExceptionKind::UNKNOWN_METHOD, "valid 中文");
        let mut encoded = LinkedBytes::new();
        ThriftEncoder::default()
            .encode(value, &mut encoded)
            .unwrap();
        let encoded = encoded.concat().freeze();
        for len in 0..encoded.len() {
            assert!(
                ThriftDecoder::<ApplicationException>::default()
                    .decode(encoded.slice(..len))
                    .is_err(),
                "truncation {len}"
            );
        }
        assert!(
            ThriftDecoder::<ApplicationException>::default()
                .decode(encoded)
                .is_ok()
        );
    }

    #[test]
    fn opaque_binary_is_preserved_but_string_utf8_is_checked() {
        let encoded = Bytes::from_static(b"\0\0\0\x01\xff");
        let mut bytes = encoded.clone();
        assert_eq!(
            checked::CheckedBinaryInput::new(&mut bytes, 64, 1024)
                .read_bytes()
                .unwrap(),
            b"\xff".as_slice()
        );
        let mut bytes = encoded;
        assert!(
            checked::CheckedBinaryInput::new(&mut bytes, 64, 1024)
                .read_faststr()
                .is_err()
        );
        let bad_string = Bytes::from_static(b"\x0b\0\x01\0\0\0\x01\xff\0");
        assert!(
            ThriftDecoder::<ApplicationException>::default()
                .decode(bad_string)
                .is_err()
        );
    }

    #[test]
    fn small_values_do_not_retain_a_large_input_allocation() {
        let mut data = vec![0, 0, 0, 8];
        data.resize(128 * 1024, b'x');
        let mut bytes = Bytes::from(data);
        let original_start = bytes.as_ptr() as usize;
        let original_end = original_start + bytes.len();
        let value = checked::CheckedBinaryInput::new(&mut bytes, 64, 1024)
            .read_bytes()
            .unwrap();
        assert_eq!(value.as_ref(), b"xxxxxxxx");
        assert!(!(original_start..original_end).contains(&(value.as_ptr() as usize)));
    }

    #[test]
    fn known_and_unknown_nesting_and_container_counts_are_bounded() {
        let mut nested = vec![];
        for _ in 0..10 {
            nested.extend_from_slice(b"\x0c\0\x63");
        }
        nested.extend_from_slice(&[0; 11]);
        let nested = Bytes::from(nested);
        assert_eq!(
            ThriftDecoder::<ApplicationException>::default()
                .with_max_depth(4)
                .decode(nested.clone())
                .unwrap_err()
                .code(),
            Code::ResourceExhausted
        );
        assert!(
            ThriftDecoder::<ApplicationException>::default()
                .decode(nested)
                .is_ok()
        );
        let mut large = vec![15, 0, 99, 10];
        large.extend_from_slice(&65536u32.to_be_bytes());
        large.resize(large.len() + 65536 * 8, 0);
        large.push(0);
        let bytes = Bytes::from(large);
        assert!(
            ThriftDecoder::<ApplicationException>::default()
                .decode(bytes.clone())
                .is_ok()
        );
        assert_eq!(
            ThriftDecoder::<ApplicationException>::default()
                .with_max_container_len(32)
                .decode(bytes)
                .unwrap_err()
                .code(),
            Code::ResourceExhausted
        );
    }
}
