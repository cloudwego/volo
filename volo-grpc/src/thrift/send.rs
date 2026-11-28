//! Thrift preparation over the shared bounded encoded queue.
use futures::{Stream, StreamExt};
use pilota::{
    LinkedBytes,
    thrift::{Message, TLengthProtocol, binary::TBinaryProtocol},
};
pub use send::{ChannelConfig, Sender};

use crate::{
    Request, Status,
    codec::{compression::CompressionEncoding, send},
    message::CodecOptions,
};

pub struct SendStream<T>(pub(crate) send::SendStream<T>);
impl<T: Message + 'static> crate::message::SendEntryMessage for SendStream<T> {
    fn payload_kind(&self) -> crate::message::PayloadKind {
        crate::message::PayloadKind::Thrift
    }
    fn into_body(
        self,
        compression: Option<CompressionEncoding>,
    ) -> crate::BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>> {
        self.into_body(compression, CodecOptions::new(Default::default()), None)
    }
    fn into_body_with_options(
        self,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
    ) -> crate::BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>> {
        self.into_body(compression, options, None)
    }
}
impl<T: Message + 'static> SendStream<T> {
    pub fn from_stream(source: impl Stream<Item = Result<T, Status>> + Send + 'static) -> Self {
        Self(send::SendStream::from_stream(source))
    }
    #[doc(hidden)]
    pub fn into_body(
        self,
        compression: Option<CompressionEncoding>,
        options: CodecOptions,
        classify: Option<fn(&T) -> bool>,
    ) -> crate::BoxStream<'static, Result<http_body::Frame<bytes::Bytes>, Status>> {
        let encoder =
            crate::codec::thrift::ThriftEncoder::default().unsafe_codec(options.unsafe_codec);
        self.0.into_body(compression, options, encoder, classify)
    }
}
pub fn channel<T: Message + 'static>(capacity: usize) -> (Sender<T>, SendStream<T>) {
    channel_with_config(ChannelConfig {
        capacity,
        ..Default::default()
    })
    .expect("channel capacity must be nonzero")
}
pub fn channel_with_config<T: Message + 'static>(
    config: ChannelConfig,
) -> Result<(Sender<T>, SendStream<T>), Status> {
    let (sender, source) = send::channel(config, select_prepare::<T>(config.unsafe_codec), None)?;
    Ok((sender, SendStream(source)))
}
pub(crate) fn response_channel<T: Message + 'static>(
    config: ChannelConfig,
    cx: crate::streaming::StreamContext,
) -> Result<(Sender<T>, SendStream<T>), Status> {
    let (sender, source) =
        send::channel(config, select_prepare::<T>(config.unsafe_codec), Some(cx))?;
    Ok((sender, SendStream(source)))
}
fn select_prepare<T: Message>(enabled: bool) -> fn(&T, usize) -> Result<LinkedBytes, Status> {
    if enabled {
        prepare::<T, true>
    } else {
        prepare::<T, false>
    }
}
fn prepare<T: Message, const UNSAFE: bool>(
    message: &T,
    limit: usize,
) -> Result<LinkedBytes, Status> {
    let mut sizing = TBinaryProtocol::new((), true);
    let size = message.size(&mut sizing);
    if size > limit {
        return Err(Status::resource_exhausted(
            "encoded message exceeds size limit",
        ));
    }
    let mut encoded = send::prepared_buffer(size.saturating_sub(sizing.zero_copy_len()));
    crate::codec::thrift::encode_message_into(message, &mut encoded, UNSAFE)?;
    if encoded.len() - crate::codec::PREFIX_LEN > limit {
        return Err(Status::resource_exhausted(
            "encoded message exceeds size limit",
        ));
    }
    Ok(encoded)
}

/// Accept a normal Rust stream or the bounded borrowed-send helper without
/// imposing the historical Protobuf `Stream: Sync` bound.
pub trait IntoStreamingRequest {
    type Message: Message + 'static;
    fn into_streaming_request(self) -> Request<SendStream<Self::Message>>;
}
impl<S, T> IntoStreamingRequest for S
where
    S: Stream<Item = T> + Send + 'static,
    T: Message + 'static,
{
    type Message = T;
    fn into_streaming_request(self) -> Request<SendStream<T>> {
        Request::new(SendStream::from_stream(self.map(Ok)))
    }
}
impl<S, T> IntoStreamingRequest for Request<S>
where
    S: Stream<Item = T> + Send + 'static,
    T: Message + 'static,
{
    type Message = T;
    fn into_streaming_request(self) -> Request<SendStream<T>> {
        self.map(|source| SendStream::from_stream(source.map(Ok)))
    }
}
impl<T: Message + 'static> IntoStreamingRequest for SendStream<T> {
    type Message = T;
    fn into_streaming_request(self) -> Request<Self> {
        Request::new(self)
    }
}
impl<T: Message + 'static> IntoStreamingRequest for Request<SendStream<T>> {
    type Message = T;
    fn into_streaming_request(self) -> Self {
        self
    }
}

#[cfg(test)]
mod prepared_tests {
    use futures::StreamExt;
    use pilota::thrift::{ApplicationException, ApplicationExceptionKind};

    use super::*;

    struct Underestimated(ApplicationException);

    impl Message for Underestimated {
        fn encode<P: pilota::thrift::TOutputProtocol>(
            &self,
            p: &mut P,
        ) -> Result<(), pilota::thrift::ThriftException> {
            self.0.encode(p)
        }

        fn decode<P: pilota::thrift::TInputProtocol>(
            p: &mut P,
        ) -> Result<Self, pilota::thrift::ThriftException> {
            ApplicationException::decode(p).map(Self)
        }

        async fn decode_async<P: pilota::thrift::TAsyncInputProtocol>(
            p: &mut P,
        ) -> Result<Self, pilota::thrift::ThriftException> {
            ApplicationException::decode_async(p).await.map(Self)
        }

        fn size<P: TLengthProtocol>(&self, _: &mut P) -> usize {
            0
        }
    }

    #[test]
    fn prepared_custom_size_hint_keeps_prefix_and_actual_size_limit() {
        for size in [0, 64, 65536] {
            let value = Underestimated(ApplicationException::new(
                ApplicationExceptionKind::INTERNAL_ERROR,
                "x".repeat(size),
            ));
            let mut expected = LinkedBytes::new();
            value
                .encode(&mut TBinaryProtocol::new(&mut expected, true))
                .unwrap();
            let len = expected.len();
            let encoded = prepare::<_, true>(&value, len).unwrap().concat();
            assert_eq!(
                &encoded[..crate::codec::PREFIX_LEN],
                &[0; crate::codec::PREFIX_LEN]
            );
            assert_eq!(
                &encoded[crate::codec::PREFIX_LEN..],
                expected.concat().as_ref()
            );
            assert_eq!(
                prepare::<_, true>(&value, len - 1).err().unwrap().code(),
                crate::Code::ResourceExhausted
            );
        }
    }

    #[tokio::test]
    async fn prepared_thrift_prefix_shares_the_encoded_field_buffer() {
        for size in [0, 128, 65536] {
            let value = ApplicationException::new(
                ApplicationExceptionKind::INTERNAL_ERROR,
                "x".repeat(size),
            );
            let len = value.size(&mut TBinaryProtocol::new((), true));
            assert!(prepare::<_, true>(&value, len - 1).is_err());
            let (mut sender, source) = channel_with_config(ChannelConfig {
                max_message_size: len,
                max_buffered_bytes: len,
                ..Default::default()
            })
            .unwrap();
            sender.send(&value).await.unwrap();
            drop(sender);
            let mut body = source.into_body(None, CodecOptions::new(Default::default()), None);
            let frame = body.next().await.unwrap().unwrap().into_data().unwrap();
            assert_eq!(frame.len(), len + crate::codec::PREFIX_LEN);
            assert_eq!(
                u32::from_be_bytes(frame[1..5].try_into().unwrap()) as usize,
                len
            );
            let decoded = ApplicationException::decode(&mut TBinaryProtocol::new(
                &mut frame.slice(5..),
                false,
            ))
            .unwrap();
            assert_eq!(decoded, value);
            assert!(body.next().await.is_none());
        }
    }
}
