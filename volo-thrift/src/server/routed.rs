//! Typed unary bindings for an external, immutable service directory.
//!
//! Only service selection is erased. Each selected Args/Result uses its original codec.

use std::{any::Any, marker::PhantomData, sync::Arc};

use bytes::Bytes;
use futures::future::BoxFuture;
use linkedbytes::LinkedBytes;
use motore::service::Service;
use pilota::thrift::{
    ApplicationException, ApplicationExceptionKind, ProtocolExceptionKind, TAsyncBinaryProtocol,
    TAsyncCompactProtocol, TAsyncInputProtocol, TInputProtocol, TLengthProtocol,
    TMessageIdentifier, TMessageType, TOutputProtocol, TType, ThriftException,
    binary::TBinaryProtocol,
    compact::{TCompactInputProtocol, TCompactOutputProtocol},
    new_protocol_exception,
};
use tokio::io::{AsyncRead, AsyncWrite};
use volo::util::buf_reader::BufReader;

use super::NamedService;
use crate::{
    EntryMessage, ServerError, ThriftMessage,
    codec::{
        Decoder, MakeCodec, StreamRead,
        default::{
            framed::DEFAULT_MAX_FRAME_SIZE,
            multiservice::decode_entry,
            thrift::{MessageReadLimit, Protocol},
        },
    },
    context::{ServerContext, ThriftContext},
};

pub type RequestBody = Box<dyn Any + Send>;
pub type BinaryInput<'r> = TAsyncBinaryProtocol<&'r mut (dyn AsyncRead + Unpin + Send + 'r)>;
pub type CompactInput<'r> = TAsyncCompactProtocol<&'r mut (dyn AsyncRead + Unpin + Send + 'r)>;

pub trait UnaryLookup: Send + Sync + 'static {
    fn find(&self, service: &str, method: &str) -> Option<Arc<dyn UnaryBinding>>;
}

/// Object-safe operations at the RPC boundary, with concrete protocols inside each binding.
pub trait UnaryBinding: Send + Sync + 'static {
    fn decode(
        &self,
        bytes: &mut Bytes,
        ident: &TMessageIdentifier,
        compact: bool,
    ) -> Result<RequestBody, ThriftException>;
    fn decode_with_config(
        &self,
        bytes: &mut Bytes,
        ident: &TMessageIdentifier,
        compact: bool,
        _unsafe_codec: bool,
    ) -> Result<RequestBody, ThriftException> {
        self.decode(bytes, ident, compact)
    }
    fn decode_binary<'a, 'r>(
        &'a self,
        protocol: &'a mut BinaryInput<'r>,
        ident: &'a TMessageIdentifier,
    ) -> BoxFuture<'a, Result<RequestBody, ThriftException>>;
    fn decode_compact<'a, 'r>(
        &'a self,
        protocol: &'a mut CompactInput<'r>,
        ident: &'a TMessageIdentifier,
    ) -> BoxFuture<'a, Result<RequestBody, ThriftException>>;
    fn call<'a>(
        &'a self,
        cx: &'a mut ServerContext,
        request: RequestBody,
    ) -> BoxFuture<'a, Result<RoutedResponse, ServerError>>;
}

pub fn bind<S, Req, Resp>(service: S) -> Arc<dyn UnaryBinding>
where
    S: Service<ServerContext, Req, Response = Resp, Error = ServerError> + Send + Sync + 'static,
    Req: EntryMessage + 'static,
    Resp: EntryMessage + 'static,
{
    Arc::new(TypedBinding(service, PhantomData::<fn(Req)>))
}

struct TypedBinding<S, Req>(S, PhantomData<fn(Req)>);

impl<S, Req, Resp> UnaryBinding for TypedBinding<S, Req>
where
    S: Service<ServerContext, Req, Response = Resp, Error = ServerError> + Send + Sync + 'static,
    Req: EntryMessage + 'static,
    Resp: EntryMessage + 'static,
{
    fn decode(
        &self,
        bytes: &mut Bytes,
        ident: &TMessageIdentifier,
        compact: bool,
    ) -> Result<RequestBody, ThriftException> {
        Ok(Box::new(decode_entry::<Req>(bytes, ident, compact)?))
    }

    fn decode_with_config(
        &self,
        bytes: &mut Bytes,
        ident: &TMessageIdentifier,
        compact: bool,
        unsafe_codec: bool,
    ) -> Result<RequestBody, ThriftException> {
        Ok(Box::new(
            crate::codec::default::multiservice::decode_entry_with_config::<Req>(
                bytes,
                ident,
                compact,
                unsafe_codec,
            )?,
        ))
    }

    fn decode_binary<'a, 'r>(
        &'a self,
        protocol: &'a mut BinaryInput<'r>,
        ident: &'a TMessageIdentifier,
    ) -> BoxFuture<'a, Result<RequestBody, ThriftException>> {
        Box::pin(async { Ok(Box::new(Req::decode_async(protocol, ident).await?) as RequestBody) })
    }

    fn decode_compact<'a, 'r>(
        &'a self,
        protocol: &'a mut CompactInput<'r>,
        ident: &'a TMessageIdentifier,
    ) -> BoxFuture<'a, Result<RequestBody, ThriftException>> {
        Box::pin(async { Ok(Box::new(Req::decode_async(protocol, ident).await?) as RequestBody) })
    }

    fn call<'a>(
        &'a self,
        cx: &'a mut ServerContext,
        request: RequestBody,
    ) -> BoxFuture<'a, Result<RoutedResponse, ServerError>> {
        Box::pin(async {
            let request = request.downcast::<Req>().map_err(|_| {
                ServerError::Application(ApplicationException::new(
                    ApplicationExceptionKind::INTERNAL_ERROR,
                    "typed request binding mismatch",
                ))
            })?;
            let response = self.0.call(cx, *request).await?;
            Ok(RoutedResponse(Box::new(TypedResponse(response))))
        })
    }
}

pub enum RoutedRequest {
    Known {
        binding: Arc<dyn UnaryBinding>,
        body: RequestBody,
    },
    Unknown,
}

pub struct RoutedResponse(Box<dyn MessageEncoder>);

pub trait MessageEncoder: Send {
    fn encode(
        &self,
        ident: &TMessageIdentifier,
        protocol: Protocol,
        bytes: &mut LinkedBytes,
    ) -> Result<(), ThriftException>;
    fn encode_with_config(
        &self,
        ident: &TMessageIdentifier,
        protocol: Protocol,
        bytes: &mut LinkedBytes,
        _unsafe_codec: bool,
    ) -> Result<(), ThriftException> {
        self.encode(ident, protocol, bytes)
    }
    fn size(
        &self,
        ident: &TMessageIdentifier,
        protocol: Protocol,
    ) -> Result<(usize, usize), ThriftException>;
}

struct TypedResponse<M>(M);

impl<M: EntryMessage> MessageEncoder for TypedResponse<M> {
    fn encode(
        &self,
        ident: &TMessageIdentifier,
        protocol: Protocol,
        bytes: &mut LinkedBytes,
    ) -> Result<(), ThriftException> {
        self.encode_with_config(ident, protocol, bytes, false)
    }
    fn encode_with_config(
        &self,
        ident: &TMessageIdentifier,
        protocol: Protocol,
        bytes: &mut LinkedBytes,
        unsafe_codec: bool,
    ) -> Result<(), ThriftException> {
        match protocol {
            Protocol::Binary => encode_binary(&self.0, ident, bytes, unsafe_codec),
            Protocol::ApacheCompact => {
                let mut p = TCompactOutputProtocol::new(bytes, true);
                p.write_message_begin(ident)?;
                self.0.encode(&mut p)?;
                p.write_message_end()
            }
            Protocol::FBThriftCompact => Err(unsupported()),
        }
    }

    fn size(
        &self,
        ident: &TMessageIdentifier,
        protocol: Protocol,
    ) -> Result<(usize, usize), ThriftException> {
        fn size<M: EntryMessage, P: TLengthProtocol>(
            msg: &M,
            ident: &TMessageIdentifier,
            p: &mut P,
        ) -> (usize, usize) {
            let total = p.message_begin_len(ident) + msg.size(p) + p.message_end_len();
            (total, total - p.zero_copy_len())
        }
        Ok(match protocol {
            Protocol::Binary => size(&self.0, ident, &mut TBinaryProtocol::new((), true)),
            Protocol::ApacheCompact => {
                size(&self.0, ident, &mut TCompactOutputProtocol::new((), true))
            }
            Protocol::FBThriftCompact => return Err(unsupported()),
        })
    }
}

fn encode_binary<M: EntryMessage>(
    msg: &M,
    ident: &TMessageIdentifier,
    bytes: &mut LinkedBytes,
    _unsafe_codec: bool,
) -> Result<(), ThriftException> {
    #[cfg(feature = "unsafe-codec")]
    if _unsafe_codec {
        let mut p = unsafe {
            let start = bytes.bytes_mut().len();
            let slice = std::slice::from_raw_parts_mut(
                bytes.bytes_mut().as_mut_ptr().add(start),
                bytes.bytes_mut().capacity() - start,
            );
            pilota::thrift::binary_unsafe::TBinaryUnsafeOutputProtocol::new(bytes, slice, true)
        };
        p.write_message_begin(ident)?;
        msg.encode(&mut p)?;
        p.write_message_end()?;
        use bytes::BufMut;
        let length = p.index();
        unsafe {
            p.buf_mut().bytes_mut().advance_mut(length);
        }
        return Ok(());
    }
    let mut p = TBinaryProtocol::new(bytes, true);
    p.write_message_begin(ident)?;
    msg.encode(&mut p)?;
    p.write_message_end()
}

fn unsupported() -> ThriftException {
    new_protocol_exception(
        ProtocolExceptionKind::NotImplemented,
        "routed message requires the contextual Thrift codec",
    )
}

fn select<Cx: ThriftContext>(
    cx: &mut Cx,
    ident: &TMessageIdentifier,
) -> Result<Option<Arc<dyn UnaryBinding>>, ThriftException> {
    cx.handle_decoded_msg_ident(ident);
    if !matches!(
        ident.message_type,
        TMessageType::Call | TMessageType::OneWay
    ) {
        return Err(new_protocol_exception(
            ProtocolExceptionKind::InvalidData,
            "expected Call or OneWay",
        ));
    }
    let lookup = cx
        .extensions()
        .get::<Arc<dyn UnaryLookup>>()
        .ok_or_else(|| {
            new_protocol_exception(
                ProtocolExceptionKind::InvalidData,
                "missing shared unary directory",
            )
        })?;
    Ok(lookup.find(
        cx.idl_service_name().map_or("", |name| name.as_str()),
        ident.name.as_str(),
    ))
}

async fn decode_buffered<Cx: ThriftContext, R: AsyncRead + Unpin + Send>(
    reader: &mut BufReader<R>,
    protocol: Protocol,
    cx: &mut Cx,
) -> Result<ThriftMessage<RoutedRequest>, ThriftException> {
    let limit = cx
        .extensions()
        .get::<MessageReadLimit>()
        .map_or(DEFAULT_MAX_FRAME_SIZE as usize, |limit| limit.0);
    let reader: &mut (dyn AsyncRead + Unpin + Send) = reader;
    let (ident, request) = match protocol {
        Protocol::Binary => {
            let mut p = TAsyncBinaryProtocol::with_read_limit(reader, limit);
            let ident = p.read_message_begin().await?;
            let binding = select(cx, &ident)?;
            let request = match binding {
                Some(binding) => {
                    let body = binding.decode_binary(&mut p, &ident).await?;
                    RoutedRequest::Known { binding, body }
                }
                None => {
                    p.skip(TType::Struct).await?;
                    RoutedRequest::Unknown
                }
            };
            p.read_message_end().await?;
            (ident, request)
        }
        Protocol::ApacheCompact => {
            let mut p = TAsyncCompactProtocol::with_read_limit(reader, limit);
            let ident = p.read_message_begin().await?;
            let binding = select(cx, &ident)?;
            let request = match binding {
                Some(binding) => {
                    let body = binding.decode_compact(&mut p, &ident).await?;
                    RoutedRequest::Known { binding, body }
                }
                None => {
                    p.skip(TType::Struct).await?;
                    RoutedRequest::Unknown
                }
            };
            p.read_message_end().await?;
            (ident, request)
        }
        Protocol::FBThriftCompact => return Err(unsupported()),
    };
    Ok(ThriftMessage::from_identifier(ident, Ok(request)))
}

impl EntryMessage for RoutedRequest {
    fn encode<P: TOutputProtocol>(&self, _: &mut P) -> Result<(), ThriftException> {
        Err(unsupported())
    }
    fn size<P: TLengthProtocol>(&self, _: &mut P) -> usize {
        unreachable!("request-only routed entry")
    }
    fn decode<P: TInputProtocol>(
        _: &mut P,
        _: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        Err(unsupported())
    }
    async fn decode_async<P: TAsyncInputProtocol>(
        _: &mut P,
        _: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        Err(unsupported())
    }

    fn decode_with_context<Cx: ThriftContext>(
        bytes: &mut Bytes,
        protocol: Protocol,
        cx: &mut Cx,
    ) -> Option<Result<ThriftMessage<Self>, ThriftException>> {
        Some((|| {
            let compact = matches!(protocol, Protocol::ApacheCompact);
            let ident = match protocol {
                Protocol::Binary => TBinaryProtocol::new(&mut *bytes, true).read_message_begin()?,
                Protocol::ApacheCompact => {
                    TCompactInputProtocol::new(&mut *bytes).read_message_begin()?
                }
                Protocol::FBThriftCompact => return Err(unsupported()),
            };
            let request = match select(cx, &ident)? {
                Some(binding) => {
                    let body = binding.decode_with_config(
                        bytes,
                        &ident,
                        compact,
                        cx.rpc_info().config().unsafe_codec(),
                    )?;
                    Self::Known { binding, body }
                }
                None => {
                    if compact {
                        TCompactInputProtocol::new(bytes).skip(TType::Struct)?;
                    } else {
                        TBinaryProtocol::new(bytes, true).skip(TType::Struct)?;
                    }
                    Self::Unknown
                }
            };
            Ok(ThriftMessage::from_identifier(ident, Ok(request)))
        })())
    }

    async fn decode_async_with_context<Cx: ThriftContext, R: AsyncRead + Unpin + Send>(
        reader: &mut BufReader<R>,
        protocol: Protocol,
        cx: &mut Cx,
    ) -> Option<Result<ThriftMessage<Self>, ThriftException>> {
        Some(decode_buffered(reader, protocol, cx).await)
    }
}

impl EntryMessage for RoutedResponse {
    fn dynamic_encoder(&self) -> Option<&dyn MessageEncoder> {
        Some(&*self.0)
    }
    fn encode<P: TOutputProtocol>(&self, _: &mut P) -> Result<(), ThriftException> {
        Err(unsupported())
    }
    fn size<P: TLengthProtocol>(&self, _: &mut P) -> usize {
        unreachable!("routed response uses its original encoder")
    }
    fn decode<P: TInputProtocol>(
        _: &mut P,
        _: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        Err(unsupported())
    }
    async fn decode_async<P: TAsyncInputProtocol>(
        _: &mut P,
        _: &TMessageIdentifier,
    ) -> Result<Self, ThriftException> {
        Err(unsupported())
    }
}

#[derive(Clone, Copy)]
pub struct RoutedService;

impl NamedService for RoutedService {
    const NAME: &'static str = "StreamX";
}

impl Service<ServerContext, RoutedRequest> for RoutedService {
    type Response = RoutedResponse;
    type Error = ServerError;

    async fn call(
        &self,
        cx: &mut ServerContext,
        request: RoutedRequest,
    ) -> Result<Self::Response, Self::Error> {
        match request {
            RoutedRequest::Known { binding, body } => binding.call(cx, body).await,
            RoutedRequest::Unknown => Err(ServerError::Application(ApplicationException::new(
                ApplicationExceptionKind::UNKNOWN_METHOD,
                format!("unknown service or method: {}", cx.rpc_info.method()),
            ))),
        }
    }
}

#[derive(Clone)]
pub struct RoutedMakeCodec<M> {
    inner: M,
    lookup: Arc<dyn UnaryLookup>,
}

impl<M> RoutedMakeCodec<M> {
    pub fn new(inner: M, lookup: Arc<dyn UnaryLookup>) -> Self {
        Self { inner, lookup }
    }
}

impl<L, M, SP> super::Server<RoutedService, L, RoutedRequest, M, SP> {
    #[doc(hidden)]
    pub fn with_directory(
        self,
        lookup: Arc<dyn UnaryLookup>,
        streams: Option<crate::transport::ttstream::router::StreamRouter>,
    ) -> Result<
        super::Server<RoutedService, L, RoutedRequest, RoutedMakeCodec<M>, SP>,
        std::io::Error,
    > {
        if self.streaming_router.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "register StreamX services in its shared directory",
            ));
        }
        let mut server = self.map_codec(|codec| RoutedMakeCodec::new(codec, lookup));
        server.streaming_router = streams;
        Ok(server)
    }
}

pub struct RoutedDecoder<D> {
    inner: D,
    lookup: Arc<dyn UnaryLookup>,
}

impl<M, R, W> MakeCodec<R, W> for RoutedMakeCodec<M>
where
    M: MakeCodec<R, W>,
    R: AsyncRead + Unpin + Send + Sync + 'static,
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    type Encoder = M::Encoder;
    type Decoder = RoutedDecoder<M::Decoder>;

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }
    fn supports_buffered(&self) -> bool {
        self.inner.supports_buffered()
    }
    fn make_codec(&self, reader: R, writer: W) -> (Self::Encoder, Self::Decoder) {
        let (encoder, inner) = self.inner.make_codec(reader, writer);
        (
            encoder,
            RoutedDecoder {
                inner,
                lookup: self.lookup.clone(),
            },
        )
    }
    fn make_codec_from_buffered(
        &self,
        reader: BufReader<R>,
        writer: W,
    ) -> Result<(Self::Encoder, Self::Decoder), std::io::Error> {
        let (encoder, inner) = self.inner.make_codec_from_buffered(reader, writer)?;
        Ok((
            encoder,
            RoutedDecoder {
                inner,
                lookup: self.lookup.clone(),
            },
        ))
    }
}

impl<D: Decoder> Decoder for RoutedDecoder<D> {
    async fn decode<M: EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<ThriftMessage<M>>, ThriftException> {
        cx.extensions_mut().insert(self.lookup.clone());
        self.inner.decode(cx).await
    }
    async fn probe_ttstream(&mut self) -> Result<bool, std::io::Error> {
        self.inner.probe_ttstream().await
    }
    fn into_stream_reader(self) -> Result<Box<dyn StreamRead>, Self> {
        match self.inner.into_stream_reader() {
            Ok(reader) => Ok(reader),
            Err(inner) => Err(Self {
                inner,
                lookup: self.lookup,
            }),
        }
    }
    async fn is_closed(&self) -> bool {
        self.inner.is_closed().await
    }
    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
        self.inner.shmipc_helper()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct SendOnly {
        payload: Bytes,
        value: Cell<u8>,
    }

    impl EntryMessage for SendOnly {
        fn encode<P: TOutputProtocol>(&self, p: &mut P) -> Result<(), ThriftException> {
            p.write_bytes(self.payload.clone())?;
            p.write_byte(self.value.get())
        }
        fn size<P: TLengthProtocol>(&self, p: &mut P) -> usize {
            p.bytes_len(&self.payload) + p.byte_len(self.value.get())
        }
        fn decode<P: TInputProtocol>(
            _: &mut P,
            _: &TMessageIdentifier,
        ) -> Result<Self, ThriftException> {
            unreachable!()
        }
        async fn decode_async<P: TAsyncInputProtocol>(
            _: &mut P,
            _: &TMessageIdentifier,
        ) -> Result<Self, ThriftException> {
            unreachable!()
        }
    }

    #[test]
    fn erased_response_preserves_zero_copy_without_requiring_sync() {
        let ident = TMessageIdentifier::new("Reply".into(), TMessageType::Reply, -3);
        let message: Box<dyn MessageEncoder> = Box::new(TypedResponse(SendOnly {
            payload: Bytes::from(vec![42; 8192]),
            value: Cell::new(7),
        }));
        for protocol in [Protocol::Binary, Protocol::ApacheCompact] {
            let (size, allocation) = message.size(&ident, protocol).unwrap();
            if matches!(protocol, Protocol::Binary) {
                assert!(allocation < size);
            }
            let mut encoded = LinkedBytes::new();
            encoded.reserve(allocation);
            message.encode(&ident, protocol, &mut encoded).unwrap();
            assert!(
                encoded.bytes_mut().len() < size,
                "payload must remain in an inserted Bytes segment"
            );
            let mut bytes = encoded.into_bytes_mut().freeze();
            assert_eq!(bytes.len(), size);
            fn check<P: TInputProtocol>(mut p: P) {
                let id = p.read_message_begin().unwrap();
                assert_eq!(id.name, "Reply");
                assert_eq!(id.sequence_number, -3);
                assert_eq!(p.read_bytes().unwrap().as_ref(), &[42; 8192]);
                assert_eq!(p.read_byte().unwrap(), 7);
            }
            match protocol {
                Protocol::Binary => check(TBinaryProtocol::new(&mut bytes, true)),
                Protocol::ApacheCompact => check(TCompactInputProtocol::new(&mut bytes)),
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod runtime_codec_tests {
    use pilota::thrift::Message;

    use super::*;
    use crate::codec::default::{
        ZeroCopyDecoder, ZeroCopyEncoder, multiservice, thrift::ThriftCodec,
    };

    struct Probe {
        value: ApplicationException,
        optimized: bool,
    }
    impl EntryMessage for Probe {
        fn encode<P: TOutputProtocol>(&self, p: &mut P) -> Result<(), ThriftException> {
            assert_eq!(
                std::any::type_name::<P>().contains("binary_unsafe"),
                self.optimized
            );
            self.value.encode(p)
        }
        fn size<P: TLengthProtocol>(&self, p: &mut P) -> usize {
            self.value.size(p)
        }
        fn decode<P: TInputProtocol>(
            p: &mut P,
            _: &TMessageIdentifier,
        ) -> Result<Self, ThriftException> {
            Ok(Self {
                value: ApplicationException::decode(p)?,
                optimized: std::any::type_name::<P>().contains("binary_unsafe"),
            })
        }
        async fn decode_async<P: TAsyncInputProtocol>(
            _: &mut P,
            _: &TMessageIdentifier,
        ) -> Result<Self, ThriftException> {
            unreachable!()
        }
    }

    #[test]
    fn runtime_codec_covers_static_and_erased_unary_and_multiservice_bodies() {
        let ident = TMessageIdentifier::new("Reply".into(), TMessageType::Reply, 1);
        let value = ApplicationException::new(ApplicationExceptionKind::UNKNOWN, "x".repeat(8192));
        let mut reference = None;
        for setting in [None, Some(false), Some(true), None] {
            let enabled = setting.unwrap_or(false);
            let optimized = enabled && cfg!(feature = "unsafe-codec");
            let mut cx = ServerContext::default();
            cx.rpc_info.config_mut().set_unsafe_codec(setting);
            let mut codec = ThriftCodec::default();
            let make = || Probe {
                value: value.clone(),
                optimized,
            };
            let message = ThriftMessage::from_identifier(ident.clone(), Ok(make()));
            let (size, reserve) = codec.size(&mut cx, &message).unwrap();
            let mut output = LinkedBytes::with_capacity(reserve);
            codec.encode(&mut cx, &mut output, message).unwrap();
            let bytes = output.concat().freeze();
            assert_eq!(bytes.len(), size);
            let baseline = reference.get_or_insert_with(|| bytes.clone());
            assert_eq!(&bytes, baseline);
            let decoded = codec
                .decode::<Probe, _>(&mut cx, &mut bytes.clone())
                .unwrap()
                .unwrap()
                .data
                .unwrap();
            assert_eq!(decoded.optimized, optimized);
            assert_eq!(decoded.value, value);

            let body = multiservice::encode_entry_with_config(&make(), false, enabled).unwrap();
            let decoded = multiservice::decode_entry_with_config::<Probe>(
                &mut body.clone(),
                &ident,
                false,
                enabled,
            )
            .unwrap();
            assert_eq!(decoded.optimized, optimized);
            assert_eq!(decoded.value, value);

            let erased = ThriftMessage::from_identifier(
                ident.clone(),
                Ok(RoutedResponse(Box::new(TypedResponse(make())))),
            );
            let (_, reserve) = codec.size(&mut cx, &erased).unwrap();
            let mut output = LinkedBytes::with_capacity(reserve);
            codec.encode(&mut cx, &mut output, erased).unwrap();
            assert_eq!(output.concat(), bytes);

            if setting.is_none() {
                let body = multiservice::encode_entry(&make(), false).unwrap();
                let decoded =
                    multiservice::decode_entry::<Probe>(&mut body.clone(), &ident, false).unwrap();
                assert!(!decoded.optimized);
                assert_eq!(decoded.value, value);
                let mut output = LinkedBytes::with_capacity(reserve);
                TypedResponse(make())
                    .encode(&ident, Protocol::Binary, &mut output)
                    .unwrap();
                assert_eq!(output.concat(), bytes);
            }
        }
    }
}
