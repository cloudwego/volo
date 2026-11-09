use bytes::{Bytes, BytesMut};
use pilota::thrift::{
    Message, TInputProtocol, TMessageIdentifier, TMessageType, TOutputProtocol,
    binary::TBinaryProtocol,
};

use super::StreamError;

pub fn encode_message<T: Message>(message: &T) -> Result<Bytes, StreamError> {
    let mut sizing = TBinaryProtocol::new(Bytes::new(), false);
    let len = message.size(&mut sizing);
    let mut bytes = BytesMut::with_capacity(len);
    message
        .encode(&mut TBinaryProtocol::new(&mut bytes, false))
        .map_err(|e| StreamError::Protocol(e.to_string()))?;
    Ok(bytes.freeze())
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
    use pilota::thrift::{ApplicationException, ApplicationExceptionKind};

    use super::*;
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
