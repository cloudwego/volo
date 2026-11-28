//! Quiet I10 handlers. Counters and echo semantics are identical on both entries.
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::StreamExt;
use serde_json::{Value, json};
use volo::{
    net::{
        conn::Conn,
        incoming::{DefaultIncoming, Incoming, MakeIncoming},
    },
    streaming::{RequestStream, ResponseSender, RpcResult, StreamContext},
};

use crate::{grpc::chat as grpc, protobuf::streamx::presence as pb, thrift::chat};

#[derive(Debug, Default)]
pub struct Counts {
    calls: AtomicUsize,
    messages: AtomicUsize,
    active: AtomicUsize,
    accepted: AtomicUsize,
}
impl Counts {
    fn enter(self: &Arc<Self>) -> Guard {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
        Guard(self.clone())
    }
    fn message(&self) {
        self.messages.fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self, handle: Option<&volo_streamx::ServerHandle>) -> Value {
        let native = handle.map(|h| {
            let s = h.stats();
            json!({"active":s.active,"calls":s.active_calls,"tasks":s.active_tasks,
                "accepted":s.accepted,"detected":s.detected,"rejected":s.rejected,"driver_errors":s.driver_errors})
        });
        json!({"calls":self.calls.load(Ordering::Relaxed),"messages":self.messages.load(Ordering::Relaxed),
            "handlers":self.active.load(Ordering::Relaxed),"accepted":self.accepted.load(Ordering::Relaxed),
            "runtime_tasks":tokio::runtime::Handle::current().metrics().num_alive_tasks(),
            "fds":std::fs::read_dir("/proc/self/fd").map(|v|v.count()).ok(),"native":native})
    }
}
struct Guard(Arc<Counts>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct Echo(pub Arc<Counts>);

// Original ordinary Thrift trait; streaming methods already share the new API.
impl chat::ChatService for Echo {
    async fn get(
        &self,
        value: chat::Message,
    ) -> Result<
        volo_thrift::MaybeException<chat::Message, chat::ChatServiceGetException>,
        volo_thrift::ServerError,
    > {
        let _guard = self.0.enter();
        self.0.message();
        Ok(volo_thrift::MaybeException::Ok(value))
    }
    async fn ping(&self) -> Result<(), volo_thrift::ServerError> {
        let _guard = self.0.enter();
        Ok(())
    }
    async fn add(&self, left: i32, right: i64) -> Result<i64, volo_thrift::ServerError> {
        let _guard = self.0.enter();
        Ok(i64::from(left) + right)
    }
    async fn fire(&self, _: chat::Message) -> Result<(), volo_thrift::ServerError> {
        let _guard = self.0.enter();
        self.0.message();
        Ok(())
    }
    async fn upload(
        &self,
        cx: StreamContext,
        input: RequestStream<chat::Message>,
    ) -> RpcResult<chat::Message> {
        chat::chat_service_streamx::Handler::upload(self, cx, input).await
    }
    async fn watch(
        &self,
        cx: StreamContext,
        value: chat::Message,
        output: ResponseSender<chat::Message>,
    ) -> RpcResult<()> {
        chat::chat_service_streamx::Handler::watch(self, cx, value, output).await
    }
    async fn talk(
        &self,
        cx: StreamContext,
        input: RequestStream<chat::Message>,
        output: ResponseSender<chat::Message>,
    ) -> RpcResult<()> {
        chat::chat_service_streamx::Handler::talk(self, cx, input, output).await
    }
}
impl grpc::ChatService for Echo {
    async fn get(
        &self,
        value: volo_grpc::Request<grpc::ChatServiceGetRequest>,
    ) -> Result<
        volo_grpc::Response<Result<chat::Message, chat::ChatServiceGetException>>,
        volo_grpc::Status,
    > {
        let _guard = self.0.enter();
        self.0.message();
        Ok(volo_grpc::Response::new(Ok(value.into_inner().request)))
    }
    async fn ping(
        &self,
        _: volo_grpc::Request<grpc::ChatServicePingRequest>,
    ) -> Result<volo_grpc::Response<()>, volo_grpc::Status> {
        let _guard = self.0.enter();
        Ok(volo_grpc::Response::new(()))
    }
    async fn add(
        &self,
        value: volo_grpc::Request<grpc::ChatServiceAddRequest>,
    ) -> Result<volo_grpc::Response<i64>, volo_grpc::Status> {
        let _guard = self.0.enter();
        let v = value.into_inner();
        Ok(volo_grpc::Response::new(i64::from(v.left) + v.right))
    }
    async fn upload(
        &self,
        cx: StreamContext,
        input: RequestStream<chat::Message>,
    ) -> RpcResult<chat::Message> {
        chat::chat_service_streamx::Handler::upload(self, cx, input).await
    }
    async fn watch(
        &self,
        cx: StreamContext,
        value: chat::Message,
        output: ResponseSender<chat::Message>,
    ) -> RpcResult<()> {
        chat::chat_service_streamx::Handler::watch(self, cx, value, output).await
    }
    async fn talk(
        &self,
        cx: StreamContext,
        input: RequestStream<chat::Message>,
        output: ResponseSender<chat::Message>,
    ) -> RpcResult<()> {
        chat::chat_service_streamx::Handler::talk(self, cx, input, output).await
    }
}

impl chat::chat_service_streamx::Handler for Echo {
    async fn get(
        &self,
        _: StreamContext,
        value: chat::Message,
    ) -> RpcResult<Result<chat::Message, chat::ChatServiceGetException>> {
        let _guard = self.0.enter();
        self.0.message();
        Ok(Ok(value))
    }
    async fn ping(&self, _: StreamContext, _: ()) -> RpcResult<()> {
        let _guard = self.0.enter();
        Ok(())
    }
    async fn add(&self, _: StreamContext, v: chat::ChatServiceAddArgsRecv) -> RpcResult<i64> {
        let _guard = self.0.enter();
        Ok(i64::from(v.left) + v.right)
    }
    async fn fire(&self, _: StreamContext, _: chat::Message) -> RpcResult<()> {
        let _guard = self.0.enter();
        self.0.message();
        Ok(())
    }
    async fn upload(
        &self,
        _: StreamContext,
        mut input: RequestStream<chat::Message>,
    ) -> RpcResult<chat::Message> {
        let _guard = self.0.enter();
        let mut last = chat::Message {
            text: "".into(),
            sequence: 0,
        };
        while let Some(value) = input.recv().await? {
            self.0.message();
            last = value;
        }
        Ok(last)
    }
    async fn watch(
        &self,
        _: StreamContext,
        value: chat::Message,
        mut output: ResponseSender<chat::Message>,
    ) -> RpcResult<()> {
        let _guard = self.0.enter();
        self.0.message();
        output.send(&value).await
    }
    async fn talk(
        &self,
        _: StreamContext,
        mut input: RequestStream<chat::Message>,
        mut output: ResponseSender<chat::Message>,
    ) -> RpcResult<()> {
        let _guard = self.0.enter();
        while let Some(value) = input.recv().await? {
            self.0.message();
            output.send(&value).await?;
        }
        Ok(())
    }
}
impl pb::presence_service_streamx::Handler for Echo {
    async fn get(&self, _: StreamContext, value: pb::Event) -> RpcResult<pb::Event> {
        let _guard = self.0.enter();
        self.0.message();
        Ok(value)
    }
    async fn upload(
        &self,
        _: StreamContext,
        mut input: RequestStream<pb::Event>,
    ) -> RpcResult<pb::Event> {
        let _guard = self.0.enter();
        let mut last = pb::Event::default();
        while let Some(value) = input.recv().await? {
            self.0.message();
            last = value;
        }
        Ok(last)
    }
    async fn watch(
        &self,
        _: StreamContext,
        value: pb::Event,
        mut output: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        let _guard = self.0.enter();
        self.0.message();
        output.send(&value).await
    }
    async fn talk(
        &self,
        _: StreamContext,
        mut input: RequestStream<pb::Event>,
        mut output: ResponseSender<pb::Event>,
    ) -> RpcResult<()> {
        let _guard = self.0.enter();
        while let Some(value) = input.recv().await? {
            self.0.message();
            output.send(&value).await?;
        }
        Ok(())
    }
}

// Published Protobuf baseline: native RecvStream/BoxStream, no StreamX adapter.
impl pb::PresenceService for Echo {
    async fn get(
        &self,
        input: volo_grpc::Request<pb::Event>,
    ) -> Result<volo_grpc::Response<pb::Event>, volo_grpc::Status> {
        let _guard = self.0.enter();
        self.0.message();
        Ok(volo_grpc::Response::new(input.into_inner()))
    }
    async fn upload(
        &self,
        input: volo_grpc::Request<volo_grpc::RecvStream<pb::Event>>,
    ) -> Result<volo_grpc::Response<pb::Event>, volo_grpc::Status> {
        let _guard = self.0.enter();
        let mut input = input.into_inner();
        let mut last = pb::Event::default();
        while let Some(value) = input.message().await? {
            self.0.message();
            last = value;
        }
        Ok(volo_grpc::Response::new(last))
    }
    async fn watch(
        &self,
        input: volo_grpc::Request<pb::Event>,
    ) -> Result<
        volo_grpc::Response<volo_grpc::BoxStream<'static, Result<pb::Event, volo_grpc::Status>>>,
        volo_grpc::Status,
    > {
        let guard = self.0.enter();
        self.0.message();
        Ok(volo_grpc::Response::new(
            futures::stream::once(async move {
                let _guard = guard;
                Ok(input.into_inner())
            })
            .boxed(),
        ))
    }
    async fn talk(
        &self,
        input: volo_grpc::Request<volo_grpc::RecvStream<pb::Event>>,
    ) -> Result<
        volo_grpc::Response<volo_grpc::BoxStream<'static, Result<pb::Event, volo_grpc::Status>>>,
        volo_grpc::Status,
    > {
        let guard = self.0.enter();
        let stream = futures::stream::try_unfold(
            (input.into_inner(), guard),
            |(mut input, guard)| async move {
                match input.message().await? {
                    Some(value) => {
                        guard.0.message();
                        Ok(Some((value, (input, guard))))
                    }
                    None => Ok(None),
                }
            },
        );
        Ok(volo_grpc::Response::new(stream.boxed()))
    }
}

#[derive(Debug)]
pub struct CountIncoming {
    pub inner: DefaultIncoming,
    pub counts: Arc<Counts>,
}
impl MakeIncoming for CountIncoming {
    type Incoming = Self;
    async fn make_incoming(self) -> io::Result<Self> {
        Ok(self)
    }
}
impl Incoming for CountIncoming {
    async fn accept(&mut self) -> io::Result<Option<Conn>> {
        let value = self.inner.accept().await?;
        if value.is_some() {
            self.counts.accepted.fetch_add(1, Ordering::Relaxed);
        }
        Ok(value)
    }
}
