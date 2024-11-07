use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceId};
use std::fmt::Debug;
use std::str::FromStr;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use volo::context::Context;
use volo::{Layer, Service};

use crate::metadata::{KeyRef, MetadataKey, MetadataValue};
use crate::{Request, Response};

impl opentelemetry::propagation::Extractor for crate::metadata::MetadataMap {
    fn get(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.keys()
            .filter_map(|k| match k {
                KeyRef::Ascii(k) => Some(k.as_str()),
                KeyRef::Binary(_) => None,
            })
            .collect::<Vec<_>>()
    }
}

impl opentelemetry::propagation::Injector for crate::metadata::MetadataMap {
    fn set(&mut self, key: &str, value: String) {
        self.insert(
            MetadataKey::from_str(key).unwrap(),
            MetadataValue::from_str(value.as_str()).unwrap(),
        );
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ClientTracingLayer;

impl<S> Layer<S> for ClientTracingLayer {
    type Service = ClientTracingService<S>;

    fn layer(self, inner: S) -> Self::Service {
        ClientTracingService(inner)
    }
}

#[derive(Clone, Debug)]
pub struct ClientTracingService<S>(S);

#[volo::service]
impl<Cx, ReqBody, S> Service<Cx, Request<ReqBody>> for ClientTracingService<S>
where
    S: Service<Cx, Request<ReqBody>> + Send + 'static + Sync,
    Cx: Context<Config = crate::context::Config> + 'static + Send,
    ReqBody: Send + 'static,
{
    async fn call(&self, cx: &mut Cx, mut req: Request<ReqBody>) -> Result<S::Response, S::Error> {
        let span = tracing::span!(
            tracing::Level::INFO,
            "rpc_call",
            method = cx.rpc_info().method().as_str()
        );

        let otel_cx = span.context();
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&otel_cx, req.metadata_mut());
        });

        self.0.call(cx, req).await
    }
}

pub struct ServerTracingLayer;

impl<S> Layer<S> for ServerTracingLayer {
    type Service = ServerTracingService<S>;

    fn layer(self, inner: S) -> Self::Service {
        ServerTracingService(inner)
    }
}

#[derive(Clone, Debug)]
pub struct ServerTracingService<S>(S);

#[volo::service]
impl<Cx, ReqBody, ResBody, ResErr, S> Service<Cx, Request<ReqBody>> for ServerTracingService<S>
where
    S: Service<Cx, Request<ReqBody>, Response = Response<ResBody>, Error = ResErr>
        + Send
        + 'static
        + Sync,
    Cx: Context<Config = crate::context::Config> + 'static + Send,
    ReqBody: Send + 'static,
    ResBody: Send + 'static,
    ResErr: Debug + Send + 'static,
{
    async fn call(&self, cx: &mut Cx, req: Request<ReqBody>) -> Result<S::Response, S::Error> {
        let method = cx.rpc_info().method().as_str();
        let span = tracing::span!(
            tracing::Level::INFO,
            "rpc_call",
            rpc.method = method,
            otel.name = format!("RPC {}", method),
            otel.kind = "server",
        );

        opentelemetry::global::get_text_map_propagator(|propagator| {
            let cx = propagator.extract(req.metadata());
            span.set_parent(cx);
        });

        self.0.call(cx, req).instrument(span).await
    }
}
