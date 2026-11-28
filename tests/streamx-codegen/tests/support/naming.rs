//! Typed handlers and incoming guard for naming regression tests.
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use streamx_codegen::{
    names_override::wire_override as renamed,
    names_pb::{alpha, beta},
    names_thrift::{conflicts as c, shadow},
};
use volo::net::incoming::{DefaultIncoming, MakeIncoming};
use volo_streamx::{RequestStream, ResponseSender, RpcResult, StreamContext};
#[derive(Clone, Default)]
pub struct Audit(pub Arc<Mutex<Vec<(String, String, String)>>>);
impl Audit {
    fn record(&self, label: &str, cx: &StreamContext) {
        assert_eq!(StreamContext::current().unwrap().id(), cx.id());
        self.0.lock().unwrap().push((
            label.to_owned(),
            cx.service().to_owned(),
            cx.method().to_owned(),
        ));
    }
}
pub struct Alpha(pub Audit);
impl alpha::echo_streamx::Handler for Alpha {
    async fn first(&self, cx: StreamContext, _: alpha::Value) -> RpcResult<alpha::Value> {
        self.0.record("alpha", &cx);
        Ok(alpha::Value {
            text: "alpha".into(),
        })
    }
    async fn alpha_stream(
        &self,
        cx: StreamContext,
        _: alpha::Value,
        mut output: ResponseSender<alpha::Value>,
    ) -> RpcResult<()> {
        self.0.record("alpha-stream", &cx);
        output
            .send(&alpha::Value {
                text: "alpha".into(),
            })
            .await
    }
    async fn upload(
        &self,
        cx: StreamContext,
        mut input: RequestStream<alpha::Value>,
    ) -> RpcResult<alpha::Value> {
        self.0.record("alpha-upload", &cx);
        while input.recv().await?.is_some() {}
        Ok(alpha::Value {
            text: "alpha".into(),
        })
    }
    async fn talk(
        &self,
        cx: StreamContext,
        mut input: RequestStream<alpha::Value>,
        mut output: ResponseSender<alpha::Value>,
    ) -> RpcResult<()> {
        self.0.record("alpha-talk", &cx);
        while input.recv().await?.is_some() {
            output
                .send(&alpha::Value {
                    text: "alpha".into(),
                })
                .await?;
        }
        Ok(())
    }
}
pub struct Beta;
impl beta::echo_streamx::Handler for Beta {
    async fn second(&self, _: StreamContext, value: beta::Value) -> RpcResult<beta::Value> {
        Ok(value)
    }
    async fn beta_stream(
        &self,
        _: StreamContext,
        _: beta::Value,
        _: ResponseSender<beta::Value>,
    ) -> RpcResult<()> {
        Ok(())
    }
}
pub struct Shadow;
impl shadow::echo_streamx::Handler for Shadow {
    async fn other(&self, _: StreamContext, value: shadow::Value) -> RpcResult<shadow::Value> {
        Ok(value)
    }
    async fn notify(&self, _: StreamContext, _: shadow::Value) -> RpcResult<()> {
        Ok(())
    }
}
pub struct Renamed(pub Audit);
impl renamed::renamed_streamx::Handler for Renamed {
    async fn call(&self, cx: StreamContext, _: renamed::Value) -> RpcResult<renamed::Value> {
        self.0.record("renamed", &cx);
        Ok(renamed::Value {
            text: "renamed".into(),
        })
    }
}
pub struct Handler {
    label: &'static str,
    audit: Audit,
}
impl Handler {
    fn reply(&self, cx: &StreamContext) -> c::Value {
        self.audit.record(self.label, cx);
        c::Value {
            text: self.label.into(),
        }
    }
}
macro_rules! handlers {
    ($($module:ident: [$($method:ident),*]),*) => { $(
        impl c::$module::Handler for Handler { $(async fn $method(&self, cx: StreamContext, _: c::Value) -> RpcResult<c::Value> { Ok(self.reply(&cx)) })* }
    )* };
}
handlers!(first_service_streamx: [shared_a, shared_b, only_a], second_service_streamx: [shared_a, shared_b, only_b], partial_fallback_streamx: [shared_a], full_fallback_streamx: [shared_a, shared_b]);
pub fn handler(label: &'static str, audit: &Audit) -> Handler {
    Handler {
        label,
        audit: audit.clone(),
    }
}
pub fn base(audit: &Audit) -> volo_streamx::Server {
    volo_streamx::Server::new()
        .add_service(c::first_service_streamx::bind(handler("first", audit)))
        .add_service(c::second_service_streamx::bind(handler("second", audit)))
}
pub struct MustNotListen(pub Arc<AtomicUsize>);
impl MakeIncoming for MustNotListen {
    type Incoming = DefaultIncoming;
    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(io::Error::other("unexpected make_incoming"))
    }
}
