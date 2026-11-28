use std::sync::{Arc, Mutex};

use futures::{StreamExt, stream};
use http_body_util::BodyExt;
use pilota::thrift::{
    ApplicationException, ApplicationExceptionKind, Message, TAsyncInputProtocol, TInputProtocol,
    TLengthProtocol, TOutputProtocol, ThriftException, binary::TBinaryProtocol,
};

use super::*;
use crate::{
    message::{CodecOptions, PayloadKind, RecvEntryMessage},
    thrift::{ChannelConfig, SendStream},
};

struct Probe {
    value: ApplicationException,
    encoders: Arc<Mutex<Vec<bool>>>,
}
impl Message for Probe {
    fn encode<P: TOutputProtocol>(&self, p: &mut P) -> Result<(), ThriftException> {
        self.encoders
            .lock()
            .unwrap()
            .push(std::any::type_name::<P>().contains("binary_output"));
        self.value.encode(p)
    }
    fn size<P: TLengthProtocol>(&self, p: &mut P) -> usize {
        self.value.size(p)
    }
    fn decode<P: TInputProtocol>(_: &mut P) -> Result<Self, ThriftException> {
        unreachable!()
    }
    async fn decode_async<P: TAsyncInputProtocol>(_: &mut P) -> Result<Self, ThriftException> {
        unreachable!()
    }
}
struct Input;
impl RecvEntryMessage for Input {
    fn payload_kind() -> PayloadKind {
        PayloadKind::Thrift
    }
    fn from_body(
        _: Option<&str>,
        _: BoxBody,
        _: crate::codec::decode::Kind,
        _: Option<crate::codec::compression::CompressionEncoding>,
    ) -> Result<Self, Status> {
        Ok(Self)
    }
}
#[derive(Clone)]
struct Echo {
    prepared: bool,
    encoders: Arc<Mutex<Vec<bool>>>,
}
impl NamedService for Echo {
    const NAME: &'static str = "codec.Echo";
}
impl Service<ServerContext, Request<Input>> for Echo {
    type Response = Response<SendStream<Probe>>;
    type Error = Status;
    async fn call(
        &self,
        _: &mut ServerContext,
        req: Request<Input>,
    ) -> Result<Self::Response, Status> {
        let options = req.extensions().get::<CodecOptions>().unwrap();
        let value = Probe {
            value: ApplicationException::new(
                ApplicationExceptionKind::UNKNOWN,
                "same wire payload",
            ),
            encoders: self.encoders.clone(),
        };
        if self.prepared {
            let (mut sender, stream) =
                crate::thrift::channel_with_config(ChannelConfig::for_options(options))?;
            sender.send(&value).await?;
            drop(sender);
            Ok(Response::new(stream))
        } else {
            Ok(Response::new(SendStream::from_stream(stream::once(
                async { Ok(value) },
            ))))
        }
    }
}

#[tokio::test]
async fn runtime_codec_is_local_to_each_server_and_reaches_prepared_and_lazy_responses() {
    for prepared in [false, true] {
        let encoders = Arc::new(Mutex::new(Vec::new()));
        let make = || {
            ServiceBuilder::new(Echo {
                prepared,
                encoders: encoders.clone(),
            })
            .build::<Input, SendStream<Probe>>()
        };
        // Exercise both builder orders and a layer that changes the server type.
        let default = Server::new().add_service(make());
        let enabled = Server::new()
            .unsafe_codec(true)
            .layer(volo::layer::Identity::new())
            .add_service(make());
        let disabled = Server::new().add_service(make()).unsafe_codec(false);
        for (server, expected) in [
            (&default.router, false),
            (&enabled.router, cfg!(feature = "unsafe-codec")),
            (&disabled.router, false),
            (&enabled.router, cfg!(feature = "unsafe-codec")),
        ] {
            let mut cx = ServerContext::default();
            cx.rpc_info.set_method("/codec.Echo/Call".into());
            let mut req = Request::new(crate::body::empty_body());
            req.metadata_mut()
                .insert("content-type", "application/grpc+thrift".parse().unwrap());
            let response = server.call(&mut cx, req).await.unwrap();
            let mut body = response.into_inner();
            let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
            let decoded = ApplicationException::decode(&mut TBinaryProtocol::new(
                &mut frame.slice(5..),
                true,
            ))
            .unwrap();
            assert_eq!(decoded.message(), "same wire payload");
            assert_eq!(encoders.lock().unwrap().pop(), Some(expected));
        }
    }
}

#[tokio::test]
async fn prepared_channel_selection_is_fixed_before_send_and_preserves_wire_bytes() {
    for enabled in [false, true] {
        let encoders = Arc::new(Mutex::new(Vec::new()));
        let value = Probe {
            value: ApplicationException::new(ApplicationExceptionKind::UNKNOWN, "queued"),
            encoders: encoders.clone(),
        };
        let (mut sender, stream) = crate::thrift::channel_with_config(ChannelConfig {
            unsafe_codec: enabled,
            ..Default::default()
        })
        .unwrap();
        let sent = sender.send(&value);
        assert_eq!(
            *encoders.lock().unwrap(),
            vec![enabled && cfg!(feature = "unsafe-codec")]
        );
        sent.await.unwrap();
        drop(sender);
        // A later body option must not encode prepared bytes a second time.
        let mut body = stream.into_body(
            None,
            CodecOptions::new(Default::default()).with_unsafe_codec(!enabled),
            None,
        );
        let bytes = body.next().await.unwrap().unwrap().into_data().unwrap();
        let decoded =
            ApplicationException::decode(&mut TBinaryProtocol::new(&mut bytes.slice(5..), true))
                .unwrap();
        assert_eq!(decoded, value.value);
        assert_eq!(encoders.lock().unwrap().len(), 1);
    }
}
