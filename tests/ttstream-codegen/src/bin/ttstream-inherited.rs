use std::{
    collections::HashMap,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use ttstream_codegen::volo_gen::{
    audit::{Child, ChildClientBuilder, ChildServer, Result as ChildMessage},
    audit_parent::{Alias, ParentPlainException, Request, Response},
};
use volo::net::{
    Address,
    incoming::{DefaultIncoming, MakeIncoming},
};
use volo_thrift::{
    MaybeException, ServerError,
    streaming::{
        PoolMode, RequestStream, ResponseSender, StreamConfig, StreamContext, StreamError,
    },
};

type AnyError = Box<dyn Error + Send + Sync>;

#[derive(Default)]
struct Handler {
    calls: AtomicUsize,
}
impl Handler {
    fn called(&self, method: &str) {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        println!(
            "{}",
            sonic_rs::json!({"event":"handler","method":method,"call_index":call_index})
        );
    }
}
impl Child for Handler {
    async fn plain(
        &self,
        req: Arc<Request>,
    ) -> Result<MaybeException<Arc<Response>, ParentPlainException>, ServerError> {
        self.called("Plain");
        Ok(MaybeException::Ok(Arc::new(Response {
            value: 10000 + req.value,
        })))
    }
    async fn unary(&self, req: Arc<Request>) -> Result<Arc<Response>, ServerError> {
        self.called("Unary");
        Ok(Arc::new(Response {
            value: 20000 + req.value,
        }))
    }
    async fn upload(
        &self,
        _cx: StreamContext,
        mut input: RequestStream<Arc<Alias>>,
    ) -> Result<Arc<Response>, StreamError> {
        self.called("Upload");
        let mut count = 0;
        while input.recv().await?.is_some() {
            count += 1;
        }
        Ok(Arc::new(Response {
            value: 30000 + count,
        }))
    }
    async fn watch(
        &self,
        _cx: StreamContext,
        req: Arc<Request>,
        mut output: ResponseSender<Arc<Response>>,
    ) -> Result<(), StreamError> {
        self.called("Watch");
        for value in 0..req.value {
            output
                .send(&Arc::new(Response {
                    value: 40000 + value,
                }))
                .await?;
        }
        Ok(())
    }
    async fn chat(
        &self,
        _cx: StreamContext,
        mut input: RequestStream<Arc<ChildMessage>>,
        mut output: ResponseSender<Arc<ChildMessage>>,
    ) -> Result<(), StreamError> {
        self.called("Chat");
        while let Some(message) = input.recv().await? {
            output
                .send(&Arc::new(ChildMessage {
                    value: 50000 + message.value,
                }))
                .await?;
        }
        Ok(())
    }
    async fn watch_result(
        &self,
        _cx: StreamContext,
        req: Arc<ChildMessage>,
        mut output: ResponseSender<Arc<ChildMessage>>,
    ) -> Result<(), StreamError> {
        self.called("WatchResult");
        for value in 0..req.value {
            output
                .send(&Arc::new(ChildMessage {
                    value: 60000 + value,
                }))
                .await?;
        }
        Ok(())
    }
}
struct ReadyIncoming {
    inner: DefaultIncoming,
    addr: String,
}
impl MakeIncoming for ReadyIncoming {
    type Incoming = DefaultIncoming;
    async fn make_incoming(self) -> std::io::Result<DefaultIncoming> {
        println!(
            "{}",
            sonic_rs::json!({"event":"ready","api":"inherited","service":"Child","addr":self.addr})
        );
        Ok(self.inner)
    }
}
fn address(value: &str) -> Result<Address, AnyError> {
    if let Some(value) = value.strip_prefix("tcp://") {
        Ok(Address::from(value.parse::<std::net::SocketAddr>()?))
    } else if let Some(path) = value.strip_prefix("unix://") {
        Ok(Address::from(
            std::os::unix::net::SocketAddr::from_pathname(path)?,
        ))
    } else {
        Err("unsupported address".into())
    }
}
#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let mut args = HashMap::new();
    let mut argv = std::env::args().skip(1);
    while let Some(key) = argv.next() {
        if !["--role", "--addr", "--method", "--messages", "--pool"].contains(&key.as_str()) {
            return Err(format!("unknown option {key}").into());
        }
        args.insert(key, argv.next().ok_or("missing option value")?);
    }
    let addr = args
        .get("--addr")
        .map(String::as_str)
        .unwrap_or("tcp://127.0.0.1:0");
    match args.get("--role").map(String::as_str).unwrap_or("server") {
        "server" => {
            let incoming = match address(addr)? {
                Address::Ip(addr) => {
                    let listener = tokio::net::TcpListener::bind(addr).await?;
                    ReadyIncoming {
                        addr: format!("tcp://{}", listener.local_addr()?),
                        inner: listener.into(),
                    }
                }
                Address::Unix(addr) => ReadyIncoming {
                    addr: addr
                        .as_pathname()
                        .map(|p| format!("unix://{}", p.display()))
                        .ok_or("UDS pathname required")?,
                    inner: tokio::net::UnixListener::bind(
                        addr.as_pathname().ok_or("UDS pathname required")?,
                    )?
                    .into(),
                },
                #[cfg(feature = "shmipc")]
                _ => return Err("inherited fixture supports TCP/UDS".into()),
            };
            ChildServer::new(Handler::default()).run(incoming).await?;
        }
        "client" => {
            let count = args
                .get("--messages")
                .map(|n| n.parse::<i32>())
                .transpose()?
                .unwrap_or(3);
            if !(0..=4096).contains(&count) {
                return Err("messages outside 0..=4096".into());
            }
            let pool = match args.get("--pool").map(String::as_str).unwrap_or("long") {
                "short" => PoolMode::Short,
                "long" => PoolMode::Long,
                "mux" => PoolMode::Multiplex,
                _ => return Err("unsupported pool".into()),
            };
            let client = ChildClientBuilder::new("Child")
                .address(address(addr)?)
                .streaming_config(StreamConfig {
                    pool_mode: pool,
                    ..Default::default()
                })
                .build();
            let selected = args.get("--method").map(String::as_str).unwrap_or("all");
            let methods: Vec<&str> = if selected == "all" {
                vec!["Plain", "Unary", "Upload", "Watch", "Chat", "WatchResult"]
            } else {
                vec![selected]
            };
            let mut calls = Vec::new();
            for method in methods {
                let request = || {
                    Arc::new(Request {
                        value: count,
                        ..Default::default()
                    })
                };
                let responses = match method {
                    "Plain" => match client.plain(request()).await? {
                        MaybeException::Ok(value) => vec![value.value],
                        MaybeException::Exception(_) => {
                            return Err("unexpected Plain exception".into());
                        }
                    },
                    "Unary" => vec![client.unary(request()).await?.value],
                    "Upload" => {
                        let mut stream = client.upload().await?;
                        for value in 0..count {
                            stream
                                .send(&Arc::new(Alias(Request {
                                    value,
                                    ..Default::default()
                                })))
                                .await?;
                        }
                        vec![stream.close_and_recv().await?.value]
                    }
                    "Watch" => {
                        let mut stream = client.watch(request()).await?;
                        let mut values = Vec::new();
                        while let Some(value) = stream.recv().await? {
                            values.push(value.value);
                        }
                        values
                    }
                    "Chat" => {
                        let (mut tx, mut rx) = client.chat().await?.into_split();
                        for value in 0..count {
                            tx.send(&Arc::new(ChildMessage { value })).await?;
                        }
                        tx.close_send().await?;
                        let mut values = Vec::new();
                        while let Some(value) = rx.recv().await? {
                            values.push(value.value);
                        }
                        values
                    }
                    "WatchResult" => {
                        let mut stream = client
                            .watch_result(Arc::new(ChildMessage { value: count }))
                            .await?;
                        let mut values = Vec::new();
                        while let Some(value) = stream.recv().await? {
                            values.push(value.value);
                        }
                        values
                    }
                    _ => return Err(format!("unsupported method {method}").into()),
                };
                let expected: Vec<i32> = match method {
                    "Plain" => vec![10000 + count],
                    "Unary" => vec![20000 + count],
                    "Upload" => vec![30000 + count],
                    "Watch" => (0..count).map(|value| 40000 + value).collect(),
                    "Chat" => (0..count).map(|value| 50000 + value).collect(),
                    "WatchResult" => (0..count).map(|value| 60000 + value).collect(),
                    _ => unreachable!(),
                };
                if responses != expected {
                    return Err(format!(
                        "wrong inherited method response for {method}: expected {expected:?}, \
                         actual {responses:?}"
                    )
                    .into());
                }
                calls.push(sonic_rs::json!({"method":method,"responses":responses}));
            }
            if selected == "all" {
                println!(
                    "{}",
                    sonic_rs::json!({"event":"result","api":"inherited","service":"Child","method":"all","calls":calls,"ok":true})
                );
            } else {
                println!(
                    "{}",
                    sonic_rs::json!({"event":"result","api":"inherited","service":"Child","method":selected,"responses":calls[0]["responses"],"ok":true})
                );
            }
        }
        _ => return Err("unsupported role".into()),
    }
    Ok(())
}
