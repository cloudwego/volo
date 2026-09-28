use std::{
    io,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use bytes::Bytes;
use motore::service::Service;
use pilota::thrift::{
    ApplicationException, ApplicationExceptionKind, TMessageIdentifier, TMessageType,
    ThriftException, binary::TBinaryProtocol,
};
use tokio::sync::Notify;
use tracing::{Span, instrument::WithSubscriber};
use volo::context::Context;

use super::serve;
use crate::{
    EntryMessage, MessageMeta, ServerError, ThriftMessage,
    codec::{Decoder, Encoder},
    context::{ServerContext, ThriftContext},
    tracing::SpanProvider,
};

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<&'static str>>>);

impl Events {
    fn push(&self, event: &'static str) {
        self.0.lock().unwrap().push(event);
    }
}

fn assert_span(name: &str) {
    assert_eq!(Span::current().metadata().map(|m| m.name()), Some(name));
}

struct OneRequestDecoder(Option<TMessageType>);

impl Decoder for OneRequestDecoder {
    async fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
    ) -> Result<Option<ThriftMessage<Msg>>, ThriftException> {
        let Some(msg_type) = self.0.take() else {
            return Ok(None);
        };
        let ident = TMessageIdentifier::new("trace_test".into(), msg_type, 42);
        cx.handle_decoded_msg_ident(&ident);
        let mut payload = Bytes::from_static(b"request");
        let data = Msg::decode(&mut TBinaryProtocol::new(&mut payload, true), &ident)?;
        Ok(Some(ThriftMessage {
            data: Ok(data),
            meta: MessageMeta {
                msg_type,
                method: ident.name,
                seq_id: ident.sequence_number,
            },
        }))
    }
}

struct TestService {
    events: Events,
    fail: bool,
}

impl Service<ServerContext, Bytes> for TestService {
    type Response = Bytes;
    type Error = ServerError;

    async fn call(&self, cx: &mut ServerContext, req: Bytes) -> Result<Bytes, ServerError> {
        assert_span("request");
        assert_eq!(cx.rpc_info().method().as_str(), "trace_test");
        assert_eq!(req, Bytes::from_static(b"request"));
        self.events.push("service");
        tokio::task::yield_now().await;
        assert_span("request");
        if self.fail {
            Err(ServerError::Application(ApplicationException::new(
                ApplicationExceptionKind::INTERNAL_ERROR,
                "injected service error",
            )))
        } else {
            Ok(req)
        }
    }
}

struct TestEncoder {
    events: Events,
    fail: bool,
    response_type: TMessageType,
}

impl Encoder for TestEncoder {
    async fn encode<Msg: Send + EntryMessage, Cx: ThriftContext>(
        &mut self,
        cx: &mut Cx,
        msg: ThriftMessage<Msg>,
    ) -> Result<(), ThriftException> {
        assert_span("response");
        assert_eq!(cx.msg_type(), self.response_type);
        assert_eq!(msg.meta.msg_type, self.response_type);
        assert_eq!(msg.meta.seq_id, 42);
        self.events.push("encode");
        tokio::task::yield_now().await;
        assert_span("response");
        if self.fail {
            Err(io::Error::other("injected encode error").into())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
struct TestSpanProvider {
    events: Events,
    request_type: TMessageType,
    response_type: TMessageType,
}

impl SpanProvider for TestSpanProvider {
    fn on_serve(&self, cx: &ServerContext) -> Span {
        // The loop also constructs a future for EOF after the request completes.
        if cx.req_msg_type.is_none() {
            return Span::none();
        }
        assert_eq!(cx.req_msg_type, Some(self.request_type));
        assert_eq!(cx.seq_id, Some(42));
        assert_eq!(cx.rpc_info().method().as_str(), "trace_test");
        assert!(cx.stats.process_start_at().is_none());
        self.events.push("on_serve");
        tracing::info_span!("request")
    }

    fn on_encode(&self, cx: &ServerContext) -> Span {
        assert_span("request");
        assert_eq!(cx.msg_type, Some(self.response_type));
        assert!(cx.stats.process_end_at().is_some());
        self.events.push("on_encode");
        tracing::info_span!("response")
    }

    fn leave_encode(&self, _cx: &ServerContext) {
        assert_span("response");
        self.events.push("leave_encode");
    }

    fn leave_serve(&self, _cx: &ServerContext) {
        assert_span("request");
        self.events.push("leave_serve");
    }
}

async fn run_request(
    request_type: TMessageType,
    service_error: bool,
    encode_error: bool,
) -> Vec<&'static str> {
    let events = Events::default();
    let response_type = if service_error {
        TMessageType::Exception
    } else {
        TMessageType::Reply
    };
    let service = TestService {
        events: events.clone(),
        fail: service_error,
    };
    let notify = Notify::new();
    serve::<_, Bytes, Bytes, _, _, _>(
        TestEncoder {
            events: events.clone(),
            fail: encode_error,
            response_type,
        },
        OneRequestDecoder(Some(request_type)),
        notify.notified(),
        Arc::new(AtomicBool::new(false)),
        &service,
        Arc::from([]),
        None,
        TestSpanProvider {
            events: events.clone(),
            request_type,
            response_type,
        },
    )
    .with_subscriber(tracing_subscriber::registry())
    .await;
    events.0.lock().unwrap().clone()
}

#[tokio::test]
async fn request_and_response_spans_survive_pending_polls() {
    assert_eq!(
        run_request(TMessageType::Call, false, false).await,
        [
            "on_serve",
            "service",
            "on_encode",
            "encode",
            "leave_encode",
            "leave_serve"
        ]
    );
}

#[tokio::test]
async fn oneway_request_does_not_create_response_span() {
    assert_eq!(
        run_request(TMessageType::OneWay, false, false).await,
        ["on_serve", "service", "leave_serve"]
    );
}

#[tokio::test]
async fn service_error_is_encoded_in_response_span() {
    assert_eq!(
        run_request(TMessageType::Call, true, false).await,
        [
            "on_serve",
            "service",
            "on_encode",
            "encode",
            "leave_encode",
            "leave_serve"
        ]
    );
}

#[tokio::test]
async fn encode_failure_still_leaves_response_span() {
    assert_eq!(
        run_request(TMessageType::Call, false, true).await,
        ["on_serve", "service", "on_encode", "encode", "leave_encode"]
    );
}
