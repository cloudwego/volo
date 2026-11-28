use bytes::{Bytes, BytesMut};
use pilota::{
    LinkedBytes,
    thrift::{
        Message, TInputProtocol, TMessageIdentifier, TMessageType, TOutputProtocol,
        binary::TBinaryProtocol,
    },
};

use super::StreamError;

pub fn encode_message<T: Message>(message: &T) -> Result<Bytes, StreamError> {
    let mut sizing = TBinaryProtocol::new(Bytes::new(), false);
    let len = message.size(&mut sizing);
    encode_message_sized(message, len)
}

pub(crate) fn encode_message_sized<T: Message>(
    message: &T,
    len: usize,
) -> Result<Bytes, StreamError> {
    let mut bytes = BytesMut::with_capacity(len);
    encode_into(message, &mut bytes, false)?;
    Ok(bytes.freeze())
}

pub(crate) fn encode_into<T: Message>(
    message: &T,
    output: &mut BytesMut,
    _unsafe_codec: bool,
) -> Result<(), StreamError> {
    #[cfg(feature = "unsafe-codec")]
    if _unsafe_codec {
        let mut protocol = pilota::thrift::binary_output::TBinaryOutputProtocol::new(output);
        return message
            .encode(&mut protocol)
            .map_err(|e| StreamError::Protocol(e.to_string()));
    }
    let mut protocol = TBinaryProtocol::new(output, false);
    message
        .encode(&mut protocol)
        .map_err(|e| StreamError::Protocol(e.to_string()))
}

pub(crate) fn encode_linked_into<T: Message>(
    message: &T,
    output: &mut LinkedBytes,
    _unsafe_codec: bool,
) -> Result<(), StreamError> {
    #[cfg(feature = "unsafe-codec")]
    if _unsafe_codec {
        let mut protocol = pilota::thrift::binary_output::TBinaryOutputProtocol::new_linked(output);
        return message
            .encode(&mut protocol)
            .map_err(|e| StreamError::Protocol(e.to_string()));
    }
    let mut protocol = TBinaryProtocol::new(output, true);
    message
        .encode(&mut protocol)
        .map_err(|e| StreamError::Protocol(e.to_string()))
}

pub fn decode_message<T: Message>(mut bytes: Bytes) -> Result<T, StreamError> {
    let value = T::decode(&mut TBinaryProtocol::new(&mut bytes, false))
        .map_err(|e| StreamError::Protocol(e.to_string()))?;
    // Kitex FastRead consumes one struct and permits remaining payload bytes.
    // The enclosing TTStream frame already owns and bounds the entire payload.
    Ok(value)
}
pub fn encode_exception(
    method: &str,
    id: i32,
    error: &pilota::thrift::ApplicationException,
) -> Result<Bytes, StreamError> {
    let mut bytes = BytesMut::new();
    let mut p = TBinaryProtocol::new(&mut bytes, false);
    p.write_message_begin(&TMessageIdentifier::new(
        method.to_owned().into(),
        TMessageType::Exception,
        id,
    ))
    .map_err(|e| StreamError::Protocol(e.to_string()))?;
    error
        .encode(&mut p)
        .map_err(|e| StreamError::Protocol(e.to_string()))?;
    p.write_message_end()
        .map_err(|e| StreamError::Protocol(e.to_string()))?;
    Ok(bytes.freeze())
}
pub fn decode_exception(
    mut bytes: Bytes,
) -> Result<pilota::thrift::ApplicationException, StreamError> {
    let mut p = TBinaryProtocol::new(&mut bytes, false);
    let identifier = p
        .read_message_begin()
        .map_err(|e| StreamError::Protocol(e.to_string()))?;
    if identifier.message_type != TMessageType::Exception {
        return Err(StreamError::Protocol("expected EXCEPTION envelope".into()));
    }
    let value = pilota::thrift::ApplicationException::decode(&mut p)
        .map_err(|e| StreamError::Protocol(e.to_string()))?;
    p.read_message_end()
        .map_err(|e| StreamError::Protocol(e.to_string()))?;
    // Match Kitex decodeException: a valid exception may have a payload suffix.
    Ok(value)
}

#[cfg(test)]
mod tests {
    use pilota::thrift::{
        ApplicationException, ApplicationExceptionKind, TAsyncInputProtocol, TLengthProtocol,
        ThriftException,
    };

    use super::*;

    struct Underestimated(ApplicationException);

    impl Message for Underestimated {
        fn encode<P: TOutputProtocol>(&self, p: &mut P) -> Result<(), ThriftException> {
            self.0.encode(p)
        }

        fn decode<P: TInputProtocol>(p: &mut P) -> Result<Self, ThriftException> {
            ApplicationException::decode(p).map(Self)
        }

        async fn decode_async<P: TAsyncInputProtocol>(p: &mut P) -> Result<Self, ThriftException> {
            ApplicationException::decode_async(p).await.map(Self)
        }

        fn size<P: TLengthProtocol>(&self, _: &mut P) -> usize {
            0
        }
    }

    #[test]
    fn custom_size_hints_cannot_corrupt_prefixed_or_segmented_output() {
        for size in [0, 64, 65536] {
            let value = Underestimated(ApplicationException::new(
                ApplicationExceptionKind::INTERNAL_ERROR,
                "x".repeat(size),
            ));
            let mut expected = BytesMut::new();
            value
                .encode(&mut TBinaryProtocol::new(&mut expected, false))
                .unwrap();
            assert_eq!(encode_message(&value).unwrap(), expected);
            let prefix = b"existing frame header";
            let mut contiguous = BytesMut::from(prefix.as_slice());
            encode_into(&value, &mut contiguous, true).unwrap();
            let mut linked = LinkedBytes::with_capacity(prefix.len());
            bytes::BufMut::put_slice(&mut linked, prefix);
            encode_linked_into(&value, &mut linked, true).unwrap();
            let mut prefixed = BytesMut::from(prefix.as_slice());
            prefixed.extend_from_slice(&expected);
            assert_eq!(contiguous, prefixed);
            assert_eq!(linked.concat(), prefixed);
        }
    }
    #[test]
    fn valid_struct_and_exception_allow_payload_suffix_like_kitex() {
        let error = ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, "fixture");
        let data = encode_message(&error).unwrap();
        let envelope = encode_exception("Bidi", -1, &error).unwrap();
        for (bytes, exception) in [(data, false), (envelope, true)] {
            let truncated = bytes.slice(..bytes.len() - 1);
            assert!(
                if exception {
                    decode_exception(truncated)
                } else {
                    decode_message::<ApplicationException>(truncated)
                }
                .is_err()
            );
            let mut suffixed = BytesMut::from(bytes.as_ref());
            suffixed.extend_from_slice(&[0xa1, 0xb2]);
            let value = if exception {
                decode_exception(suffixed.freeze())
            } else {
                decode_message::<ApplicationException>(suffixed.freeze())
            }
            .unwrap();
            assert_eq!(value.kind(), error.kind());
            assert_eq!(value.message(), error.message());
        }
    }
}
