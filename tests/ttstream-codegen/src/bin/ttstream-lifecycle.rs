//! Explicit-control lifecycle fixture; all payloads use generated Thrift messages.
use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use sonic_rs::{JsonValueTrait, Value, json};
use ttstream_codegen::volo_gen::interop::{Interop, InteropClientBuilder, InteropServer, Message};
use volo::net::{
    Address,
    incoming::{DefaultIncoming, Incoming, MakeIncoming},
};
use volo_thrift::{
    streaming::{
        HookChain, PoolMode, RecvTimeoutPolicy, RequestStream, ResponseSender, StreamConfig,
        StreamContext, StreamError, StreamHook, StreamStatus,
    },
    transport::ttstream::connection::{Connection, StreamRouter},
};

type Error = Box<dyn std::error::Error + Send + Sync>;
fn emit(value: Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}
fn terminal(error: Option<&StreamError>) -> Value {
    match error {
        None => json!({"kind":"ok", "code":0, "path":[]}),
        Some(error) => {
            let path = match error {
                StreamError::Canceled { path, .. } => path.clone(),
                _ => Vec::new(),
            };
            json!({"kind":"error","code":error.kitex_code(),"kitex_code":error.kitex_code(),"path":path,"error":error.to_string()})
        }
    }
}
fn message(action: &str, id: &str) -> Message {
    Message {
        value: json!({"schemaVersion":1,"action":action,"id":id})
            .to_string()
            .into(),
        seq: 1,
    }
}
fn action(message: &Message) -> Result<(String, String), StreamError> {
    let value: Value =
        sonic_rs::from_str(&message.value).map_err(|e| StreamError::Protocol(e.to_string()))?;
    let field = |name| {
        value
            .get(name)
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| StreamError::Protocol(format!("missing {name}")))
    };
    Ok((field("action")?, field("id")?))
}
struct Held {
    wake: Arc<tokio::sync::Notify>,
    monitor: volo_thrift::streaming::StreamResourceMonitor,
}
static GATES: OnceLock<Mutex<BTreeMap<String, Held>>> = OnceLock::new();
fn gates() -> &'static Mutex<BTreeMap<String, Held>> {
    GATES.get_or_init(Default::default)
}
fn resources(r: volo_thrift::streaming::StreamResources) -> Value {
    json!({"queued_messages":r.queued_messages,"queued_bytes":r.queued_bytes,"high_water_messages":r.high_water_messages,"high_water_bytes":r.high_water_bytes,"read_pauses":r.read_pauses,"read_pause_nanos":r.read_pause_nanos,"control_queue_delay_nanos":r.control_queue_delay_nanos,"buffer_limit_resets":r.buffer_limit_resets})
}
struct Gate(String);
impl Drop for Gate {
    fn drop(&mut self) {
        gates().lock().unwrap().remove(&self.0);
    }
}
struct RequestId(String);
struct Observer;
impl StreamHook for Observer {
    fn on_finish(&self, cx: &StreamContext, status: &StreamStatus) {
        let id = cx.with_extensions(|ext| ext.get::<RequestId>().map(|id| id.0.clone()));
        if let Some(id) = id {
            emit(
                json!({"event":"observed","id":id,"terminal":terminal(status.error.as_ref()),"resources":resources(status.resources)}),
            );
        }
    }
}
fn attach<S>(
    socket: S,
    connection_id: usize,
    config: StreamConfig,
    all: &Arc<Mutex<Vec<Connection>>>,
) -> Result<(), StreamError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut router = StreamRouter::new();
    router.add("Interop", "Bidi", move |cx, stream| async move {
        let (input, output) = stream.into_server::<Message, Message>();
        bidi_body(cx, input, output, Some(connection_id)).await
    })?;
    let (reader, writer) = tokio::io::split(socket);
    let mut hooks = HookChain::default();
    hooks.push(Arc::new(Observer));
    let connection = Connection::start(reader, writer, config, Some(hooks), Some(router))?;
    all.lock().unwrap().push(connection);
    Ok(())
}
async fn bidi_body(
    cx: StreamContext,
    mut input: RequestStream<Message>,
    mut output: ResponseSender<Message>,
    connection: Option<usize>,
) -> Result<(), StreamError> {
    while let Some(request) = input.recv().await? {
        let (command, id) = action(&request)?;
        cx.update_extensions(|ext| {
            ext.insert(RequestId(id.clone()));
        });
        match command.as_str() {
            "hold" => {
                let gate = Arc::new(tokio::sync::Notify::new());
                if gates()
                    .lock()
                    .unwrap()
                    .insert(
                        id.clone(),
                        Held {
                            wake: gate.clone(),
                            monitor: cx.resource_monitor(),
                        },
                    )
                    .is_some()
                {
                    return Err(StreamError::configuration("duplicate hold id"));
                }
                let _guard = Gate(id.clone());
                emit(json!({"event":"started","id":id,"connection":connection}));
                output.send(&message("ack", &id)).await?;
                gate.notified().await;
                let mut count = 0;
                while input.recv().await?.is_some() {
                    count += 1;
                }
                let mut response = message("released", &id);
                response.seq = count;
                output.send(&response).await?;
                return Ok(());
            }
            "start" => {
                emit(json!({"event":"started","id":id,"connection":connection}));
                output.send(&message("ack", &id)).await?;
            }
            "release" => {
                output.send(&message("released", &id)).await?;
                return Ok(());
            }
            "probe" => {
                output.send(&message("probe", &id)).await?;
                return Ok(());
            }
            _ => return Err(StreamError::Protocol(format!("unknown action {command}"))),
        }
    }
    Ok(())
}
struct MixedHandler;
impl Interop for MixedHandler {
    async fn unary(&self, request: Message) -> Result<Message, volo_thrift::ServerError> {
        Ok(request)
    }
    async fn explicit_unary(&self, request: Message) -> Result<Message, volo_thrift::ServerError> {
        Ok(request)
    }
    async fn client(
        &self,
        _: StreamContext,
        mut input: RequestStream<Message>,
    ) -> Result<Message, StreamError> {
        let mut value = Default::default();
        let mut seq = 0;
        while let Some(request) = input.recv().await? {
            value = request.value;
            seq += 1;
        }
        Ok(Message { value, seq })
    }
    async fn server(
        &self,
        _: StreamContext,
        request: Message,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        for seq in 0..request.seq {
            output
                .send(&Message {
                    value: request.value.clone(),
                    seq,
                })
                .await?;
        }
        Ok(())
    }
    async fn bidi(
        &self,
        cx: StreamContext,
        input: RequestStream<Message>,
        output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        // The generated server does not expose connection sequence ids. Null is
        // explicit; the runner retains the actual socket for shutdown checks.
        bidi_body(cx, input, output, None).await
    }
}
struct ReadyIncoming {
    incoming: DefaultIncoming,
    address: String,
}
#[derive(Debug)]
struct CountedIncoming {
    inner: DefaultIncoming,
    accepted: usize,
}
impl Incoming for CountedIncoming {
    async fn accept(&mut self) -> std::io::Result<Option<volo::net::conn::Conn>> {
        let connection = self.inner.accept().await?;
        if connection.is_some() {
            self.accepted += 1;
            emit(json!({"event":"accepted", "connection":self.accepted}));
        }
        Ok(connection)
    }
}
impl MakeIncoming for ReadyIncoming {
    type Incoming = CountedIncoming;
    async fn make_incoming(self) -> std::io::Result<Self::Incoming> {
        emit(
            json!({"event":"ready","addr":self.address,"api":"lifecycle-mixed","schemaVersion":1}),
        );
        Ok(CountedIncoming {
            inner: self.incoming,
            accepted: 0,
        })
    }
}
fn server_config(args: &BTreeMap<String, String>) -> Result<StreamConfig, Error> {
    let mut config = StreamConfig::default();
    if let Some(value) = args.get("server-deadline-ms") {
        config.deadline = Some(Duration::from_millis(value.parse()?));
    }
    for (name, slot) in [
        ("recv-messages", &mut config.recv_messages),
        ("recv-bytes", &mut config.recv_bytes),
        ("connection-recv-bytes", &mut config.connection_recv_bytes),
        ("shared-recv-bytes", &mut config.shared_recv_bytes),
    ] {
        if let Some(value) = args.get(name) {
            *slot = value.parse()?;
        }
    }
    if let Some(value) = args.get("max-read-pause-ms") {
        config.max_read_pause = Duration::from_millis(value.parse()?);
    }
    for (name, slot) in [
        ("drain-timeout-ms", &mut config.drain_timeout),
        (
            "shutdown-flush-timeout-ms",
            &mut config.shutdown_flush_timeout,
        ),
    ] {
        if let Some(value) = args.get(name) {
            *slot = Duration::from_millis(value.parse()?);
        }
    }
    config.validate()?;
    Ok(config)
}
async fn mixed_server(args: &BTreeMap<String, String>) -> Result<(), Error> {
    let address = args
        .get("addr")
        .map(String::as_str)
        .unwrap_or("tcp://127.0.0.1:0");
    let incoming = if let Some(address) = address.strip_prefix("tcp://") {
        let listener = tokio::net::TcpListener::bind(address).await?;
        ReadyIncoming {
            address: format!("tcp://{}", listener.local_addr()?),
            incoming: listener.into(),
        }
    } else if let Some(path) = address.strip_prefix("unix://") {
        ReadyIncoming {
            address: address.to_owned(),
            incoming: tokio::net::UnixListener::bind(path)?.into(),
        }
    } else {
        return Err("unsupported address".into());
    };
    let config = server_config(args)?;
    InteropServer::new(MixedHandler)
        .streaming_config(config)
        .streaming_hook(Arc::new(Observer))
        .register_shutdown_hook(|| {
            Box::pin(async {
                // This hook runs before framework drain begins. It is a signal
                // observation only, never an acknowledgment that admission stopped.
                emit(json!({"event":"shutdown-hook"}));
            })
        })
        .run(incoming)
        .await?;
    Ok(())
}
async fn server(args: &BTreeMap<String, String>) -> Result<(), Error> {
    let addr = args
        .get("addr")
        .map(String::as_str)
        .unwrap_or("tcp://127.0.0.1:0");
    let config = server_config(args)?;
    let all = Arc::new(Mutex::new(Vec::<Connection>::new()));
    let (send, mut controls) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    enum Listener {
        Tcp(tokio::net::TcpListener),
        Unix(tokio::net::UnixListener),
    }
    let (listener, bound) = if let Some(addr) = addr.strip_prefix("tcp://") {
        let l = tokio::net::TcpListener::bind(addr).await?;
        let bound = format!("tcp://{}", l.local_addr()?);
        (Listener::Tcp(l), bound)
    } else if let Some(path) = addr.strip_prefix("unix://") {
        (
            Listener::Unix(tokio::net::UnixListener::bind(path)?),
            addr.to_owned(),
        )
    } else {
        return Err("unsupported address".into());
    };
    emit(
        json!({"event":"ready","addr":bound,"api":"lifecycle","schemaVersion":1,"config":{"recv_messages":config.recv_messages,"recv_bytes":config.recv_bytes,"connection_recv_bytes":config.connection_recv_bytes,"shared_recv_bytes":config.shared_recv_bytes,"max_read_pause_ms":config.max_read_pause.as_millis() as u64}}),
    );
    let mut connections = 0;
    loop {
        tokio::select! {
            result=async {match &listener {
                Listener::Tcp(listener)=>{let(s,_)=listener.accept().await?; attach(s, connections+1,config.clone(),&all)?;},
                Listener::Unix(listener)=>{let(s,_)=listener.accept().await?; attach(s, connections+1,config.clone(),&all)?;},
            }; Ok::<_,Error>(())}=>{result?; connections+=1;},
            Some(line)=controls.recv()=>{
                let control:Value=sonic_rs::from_str(&line)?;
                match control.get("action").as_str() {
                    Some("snapshot")=>{
                        let held:Vec<_>=gates().lock().unwrap().iter().filter_map(|(id,held)|held.monitor.snapshot().map(|r|json!({"id":id,"resources":resources(r)}))).collect();
                        let list=all.lock().unwrap();
                        emit(json!({"event":"snapshot","held":held,"connections":connections,"active_streams":list.iter().map(Connection::active_streams).sum::<usize>(),"closed_connections":list.iter().filter(|c|c.is_closed()).count()}));
                    },
                    Some("release")=>{
                        let id=control.get("id").as_str().map(str::to_owned).ok_or("missing release id")?;
                        let gate=gates().lock().unwrap().get(&id).map(|held|held.wake.clone());
                        if let Some(gate)=&gate {gate.notify_one();}
                        emit(json!({"event":"gate_released","id":id,"found":gate.is_some()}));
                    },
                    Some("drain")=>{for c in all.lock().unwrap().iter(){c.drain();} emit(json!({"event":"draining","connections":connections}));},
                    Some("close")=>{for c in all.lock().unwrap().iter(){c.close();} emit(json!({"event":"closed","connections":connections}));},
                    Some("stop")=>return Ok(()),
                    _=>return Err("unknown server control".into()),
                }
            }
        }
    }
}
async fn client(args: &BTreeMap<String, String>) -> Result<(), Error> {
    let addr = args.get("addr").ok_or("missing addr")?;
    let address = if let Some(value) = addr.strip_prefix("tcp://") {
        Address::Ip(value.parse()?)
    } else if let Some(path) = addr.strip_prefix("unix://") {
        Address::Unix(std::os::unix::net::SocketAddr::from_pathname(path)?)
    } else {
        return Err("unsupported addr".into());
    };
    let scenario = args
        .get("scenario")
        .map(String::as_str)
        .unwrap_or("keep-open");
    let id = args.get("id").map(String::as_str).unwrap_or("lifecycle");
    let wait = Duration::from_millis(
        args.get("timeout-ms")
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(30),
    );
    let client = InteropClientBuilder::new("Interop")
        .caller_name("A")
        .address(address)
        .streaming_config(StreamConfig {
            pool_mode: PoolMode::Multiplex,
            ..Default::default()
        })
        .build();
    let (mut tx, mut rx) = client.bidi().await?.into_split();
    tx.send(&message("start", id)).await?;
    let ack = rx.recv().await?.ok_or("missing ack")?;
    if action(&ack)?.0 != "ack" {
        return Err("wrong ack".into());
    }
    emit(json!({"event":"ack","id":id}));
    let mut responses = Vec::<Value>::new();
    let observed;
    let mut keep_open = false;
    match scenario {
        "keep-open" | "cancel" => {
            let policy = if scenario == "cancel" {
                RecvTimeoutPolicy::Cancel
            } else {
                RecvTimeoutPolicy::KeepOpen
            };
            let result = rx.recv_with_timeout_policy(wait, policy).await;
            observed = terminal(result.as_ref().err());
            if !matches!(result, Err(StreamError::RecvTimeout)) {
                return Err("expected receive timeout".into());
            }
            if scenario == "keep-open" {
                keep_open =
                    rx.context().cancellation().is_none() && rx.context().status().is_none();
                tx.send(&message("release", id)).await?;
                tx.close_send().await?;
                while let Some(value) = rx.recv().await? {
                    responses.push(json!({"value":value.value.as_str(),"seq":value.seq}));
                }
            }
        }
        "server-timeout" | "disconnect" => {
            let result = rx.recv().await;
            observed = terminal(result.as_ref().err());
            if result.is_ok() {
                return Err("expected terminal error".into());
            }
        }
        _ => return Err("unsupported client scenario".into()),
    }
    drop((tx, rx));
    let probe = if scenario == "disconnect" {
        false
    } else {
        let (mut tx, mut rx) = client.bidi().await?.into_split();
        tx.send(&message("probe", "probe")).await?;
        tx.close_send().await?;
        let value = rx.recv().await?.ok_or("missing probe")?;
        action(&value)?.0 == "probe" && rx.recv().await?.is_none()
    };
    emit(
        json!({"event":"result","ok":true,"api":"lifecycle","scenario":scenario,"id":id,"terminal":observed,"keep_open":keep_open,"final_terminal":if keep_open {terminal(None)} else {Value::new()},"responses":responses,"probe":probe}),
    );
    Ok(())
}
#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = BTreeMap::new();
    let mut input = std::env::args().skip(1);
    while let Some(key) = input.next() {
        let key = key
            .strip_prefix("--")
            .ok_or("expected --option")?
            .to_owned();
        if !matches!(
            key.as_str(),
            "role"
                | "addr"
                | "scenario"
                | "id"
                | "timeout-ms"
                | "server-deadline-ms"
                | "drain-timeout-ms"
                | "shutdown-flush-timeout-ms"
                | "recv-messages"
                | "recv-bytes"
                | "connection-recv-bytes"
                | "shared-recv-bytes"
                | "max-read-pause-ms"
        ) {
            return Err(format!("unknown option {key}").into());
        }
        args.insert(key, input.next().ok_or("missing option value")?);
    }
    match args.get("role").map(String::as_str) {
        Some("server") => server(&args).await,
        Some("mixed-server") => mixed_server(&args).await,
        Some("client") => tokio::time::timeout(Duration::from_secs(10), client(&args)).await?,
        _ => Err("role must be server, mixed-server or client".into()),
    }
}
