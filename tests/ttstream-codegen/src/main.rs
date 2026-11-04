mod error_status;
mod response_probe;
mod routing;

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    error::Error,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use metainfo::{Forward, METAINFO, MetaInfo};
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Value};
use ttstream_codegen::volo_gen::interop::{Interop, InteropClientBuilder, InteropServer, Message};
use volo::net::{
    Address,
    incoming::{DefaultIncoming, MakeIncoming},
};
use volo_thrift::streaming::{
    PoolMode, RequestStream, ResponseSender, StreamConfig, StreamContext, StreamError,
};

struct Handler {
    metadata_node: Option<String>,
    cascade_node: Option<String>,
    errors: bool,
    forward: Option<Address>,
}

static ACTIVE_CASCADE_HANDLERS: AtomicUsize = AtomicUsize::new(0);
static CASCADE_EVENT_ORDER: Mutex<()> = Mutex::new(());

struct CascadeGuard {
    event: &'static str,
    node: String,
    id: String,
    cx: StreamContext,
    error: Option<StreamError>,
    handler: bool,
}
impl CascadeGuard {
    fn new(event: &'static str, node: &str, id: &str, cx: &StreamContext, handler: bool) -> Self {
        if handler {
            ACTIVE_CASCADE_HANDLERS.fetch_add(1, Ordering::SeqCst);
        }
        Self {
            event,
            node: node.to_owned(),
            id: id.to_owned(),
            cx: cx.clone(),
            error: None,
            handler,
        }
    }
    fn record<T>(&mut self, result: &Result<T, StreamError>) {
        self.error = result.as_ref().err().cloned();
    }
}
impl Drop for CascadeGuard {
    fn drop(&mut self) {
        // Keep the counter snapshot and its output in the same observable order.
        let _event_order = CASCADE_EVENT_ORDER
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let active = if self.handler {
            ACTIVE_CASCADE_HANDLERS.fetch_sub(1, Ordering::SeqCst) - 1
        } else {
            ACTIVE_CASCADE_HANDLERS.load(Ordering::SeqCst)
        };
        let error = self
            .cx
            .cancellation()
            .or_else(|| self.cx.status().and_then(|status| status.error))
            .or_else(|| self.error.clone());
        let path = match &error {
            Some(StreamError::Canceled { path, .. }) => path.clone(),
            _ => Vec::new(),
        };
        let value = sonic_rs::json!({"event":self.event,"node":self.node,"id":self.id,"has_deadline":self.cx.deadline().is_some(),"error":error.as_ref().map(ToString::to_string),"code":error.as_ref().and_then(StreamError::kitex_code).unwrap_or(0),"ttscp":path,"active_handlers":active});
        // This also runs when the framework cancels and drops the handler future.
        if let Ok(line) = sonic_rs::to_string(&value) {
            println!("{line}");
        }
    }
}

fn address(value: &str) -> Result<Address, Box<dyn Error + Send + Sync>> {
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

fn string_entries(value: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let object = value.as_object().ok_or("expected a metadata object")?;
    object
        .iter()
        .map(|(key, value)| {
            Ok((
                key.to_owned(),
                value
                    .as_str()
                    .ok_or("metadata values must be strings")?
                    .to_owned(),
            ))
        })
        .collect()
}

fn metadata_error(error: impl std::fmt::Display) -> StreamError {
    StreamError::Protocol(format!("metainfo fixture: {error}"))
}

impl Handler {
    async fn cascade(
        &self,
        cx: StreamContext,
        mut input: RequestStream<Message>,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        let message = input
            .recv()
            .await?
            .ok_or_else(|| metadata_error("cascade missing request"))?;
        let request: Value = sonic_rs::from_str(&message.value).map_err(metadata_error)?;
        let id = request
            .get("id")
            .as_str()
            .ok_or_else(|| metadata_error("cascade missing id"))?
            .to_owned();
        let action = request
            .get("action")
            .as_str()
            .ok_or_else(|| metadata_error("cascade missing action"))?
            .to_owned();
        if request.get("schemaVersion").as_u64() != Some(1)
            || !matches!(action.as_str(), "cancel" | "probe")
        {
            return Err(metadata_error("invalid cascade request"));
        }
        let node = self.cascade_node.as_deref().unwrap_or("C");
        let mut guard = CascadeGuard::new("cascade_finish", node, &id, &cx, true);
        let result = async {
            if let Some(forward) = &self.forward {
                let client = InteropClientBuilder::new("Interop")
                    .caller_name(node)
                    .address(forward.clone())
                    .build();
                // Declared first so the stream halves drop before its release record.
                let mut child_guard;
                let (mut tx, mut rx) = client.bidi().await?.into_split();
                child_guard =
                    CascadeGuard::new("cascade_child_finish", node, &id, tx.context(), false);
                let result = async {
                    // The current handler scope supplies cancellation linkage. Never attach
                    // manually.
                    tx.send(&message).await?;
                    let ack = rx
                        .recv()
                        .await?
                        .ok_or_else(|| metadata_error("cascade missing downstream ack"))?;
                    output.send(&ack).await?;
                    if rx.recv().await?.is_some() {
                        return Err(metadata_error("cascade unexpected downstream message"));
                    }
                    if action == "cancel" {
                        return Err(metadata_error("cascade ended before cancellation"));
                    }
                    tx.close_send().await?;
                    Ok(())
                }
                .await;
                child_guard.record(&result);
                result
            } else {
                output.send(&message).await?;
                if action == "cancel" {
                    // Keep both directions alive until the transport propagates cancellation.
                    if input.recv().await?.is_some() {
                        return Err(metadata_error("cascade unexpected upstream message"));
                    }
                    return Err(metadata_error("cascade ended before cancellation"));
                }
                Ok(())
            }
        }
        .await;
        guard.record(&result);
        result
    }
    async fn metadata_message(
        &self,
        cx: &StreamContext,
        mut message: Message,
    ) -> Result<Message, StreamError> {
        let mut request: Value = sonic_rs::from_str(&message.value).map_err(metadata_error)?;
        if request.get("schemaVersion").as_u64() != Some(1) {
            return Err(metadata_error("unsupported schemaVersion"));
        }
        let (persistent, transient) = cx.with_metainfo(|info| {
            let mut persistent = BTreeMap::new();
            let mut transient = BTreeMap::new();
            if let Some(values) = info.get_all_persistents() {
                for (key, value) in values {
                    if key.starts_with("interop-") {
                        persistent.insert(key.to_string(), value.to_string());
                    }
                }
            }
            for values in [info.get_all_upstreams(), info.get_all_transients()]
                .into_iter()
                .flatten()
            {
                for (key, value) in values {
                    if key.starts_with("interop-") {
                        transient.insert(key.to_string(), value.to_string());
                    }
                }
            }
            (persistent, transient)
        });
        let metadata = cx.metadata();
        let string: BTreeMap<_, _> = metadata
            .string
            .iter()
            .filter_map(|(key, value)| {
                Some((
                    std::str::from_utf8(key).ok()?.to_owned(),
                    std::str::from_utf8(value).ok()?.to_owned(),
                ))
            })
            .collect();
        let int: BTreeMap<_, _> = metadata
            .int
            .iter()
            .filter_map(|(key, value)| {
                Some((key.to_string(), std::str::from_utf8(value).ok()?.to_owned()))
            })
            .collect();
        let observation = sonic_rs::json!({"node": self.metadata_node, "persistent": persistent, "transient": transient, "has_deadline": cx.deadline().is_some(), "header_string": string, "header_int": int});
        request
            .get_mut("observations")
            .and_then(|value| value.as_array_mut())
            .ok_or_else(|| metadata_error("observations must be an array"))?
            .push(observation);
        if let Some(forward) = &self.forward {
            let transient =
                string_entries(request.get("reset_transient")).map_err(metadata_error)?;
            let persistent =
                string_entries(request.get("set_persistent")).map_err(metadata_error)?;
            cx.update_metainfo(|info| {
                for (key, value) in transient {
                    info.set_transient(key, value);
                }
                for (key, value) in persistent {
                    info.set_persistent(key, value);
                }
            });
            // Rely on the framework's current handler scope and real typed client.
            // No metadata is copied from the incoming header to the outgoing call.
            let client = InteropClientBuilder::new("Interop")
                .address(forward.clone())
                .build();
            let (mut tx, mut rx) = client.bidi().await?.into_split();
            message.value = sonic_rs::to_string(&request)
                .map_err(metadata_error)?
                .into();
            tx.send(&message).await?;
            tx.close_send().await?;
            let response = rx
                .recv()
                .await?
                .ok_or_else(|| metadata_error("downstream returned no response"))?;
            if rx.recv().await?.is_some() {
                return Err(metadata_error("downstream returned extra response"));
            }
            Ok(response)
        } else {
            message.value = sonic_rs::to_string(&request)
                .map_err(metadata_error)?
                .into();
            Ok(message)
        }
    }
}
impl Interop for Handler {
    async fn unary(&self, request: Message) -> Result<Message, volo_thrift::ServerError> {
        Ok(request)
    }
    async fn explicit_unary(&self, request: Message) -> Result<Message, volo_thrift::ServerError> {
        Ok(request)
    }
    async fn client(
        &self,
        _cx: StreamContext,
        mut input: RequestStream<Message>,
    ) -> Result<Message, StreamError> {
        let mut value = Default::default();
        let mut seq = 0;
        while let Some(message) = input.recv().await? {
            value = message.value;
            seq += 1;
        }
        Ok(Message { value, seq })
    }
    async fn server(
        &self,
        _cx: StreamContext,
        req: Message,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        for seq in 0..req.seq {
            output
                .send(&Message {
                    value: req.value.clone(),
                    seq,
                })
                .await?;
        }
        Ok(())
    }
    async fn bidi(
        &self,
        cx: StreamContext,
        mut input: RequestStream<Message>,
        mut output: ResponseSender<Message>,
    ) -> Result<(), StreamError> {
        if self.errors {
            return error_status::serve(input, output).await;
        }
        if self.cascade_node.is_some() {
            return self.cascade(cx, input, output).await;
        }
        while let Some(message) = input.recv().await? {
            let message = if self.metadata_node.is_some() {
                self.metadata_message(&cx, message).await?
            } else {
                message
            };
            output.send(&message).await?;
        }
        Ok(())
    }
}
struct ReadyIncoming {
    inner: DefaultIncoming,
    address: String,
}
impl MakeIncoming for ReadyIncoming {
    type Incoming = DefaultIncoming;
    async fn make_incoming(self) -> std::io::Result<DefaultIncoming> {
        println!(
            "{{\"event\":\"ready\",\"api\":\"typed\",\"addr\":{}}}",
            sonic_rs::to_string(&self.address).unwrap()
        );
        Ok(self.inner)
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = HashMap::new();
    let mut argv = std::env::args().skip(1);
    while let Some(key) = argv.next() {
        args.insert(key, argv.next().ok_or("missing option value")?);
    }
    let role = args.get("--role").map(String::as_str).unwrap_or("server");
    if role == "routing-registration" {
        println!("{}", sonic_rs::to_string(&routing::registrations())?);
        return Ok(());
    }
    let addr = args
        .get("--addr")
        .map(String::as_str)
        .unwrap_or("tcp://127.0.0.1:18090");
    if role == "server" {
        let incoming = if let Some(addr) = addr.strip_prefix("tcp://") {
            let listener = tokio::net::TcpListener::bind(addr).await?;
            ReadyIncoming {
                address: format!("tcp://{}", listener.local_addr()?),
                inner: listener.into(),
            }
        } else if let Some(path) = addr.strip_prefix("unix://") {
            ReadyIncoming {
                address: addr.to_owned(),
                inner: tokio::net::UnixListener::bind(path)?.into(),
            }
        } else {
            return Err("unsupported address".into());
        };
        let scenario = args.get("--scenario").map(String::as_str).unwrap_or("echo");
        if scenario == "routing" {
            let profile = args
                .get("--routing-profile")
                .map(String::as_str)
                .unwrap_or("unique");
            InteropServer::new(Handler {
                metadata_node: None,
                cascade_node: None,
                errors: false,
                forward: None,
            })
            .streaming_router(routing::router(profile)?)
            .run(incoming)
            .await?;
            return Ok(());
        }
        let metadata_node = if scenario == "metainfo" {
            Some(args.get("--node").cloned().unwrap_or_else(|| "C".into()))
        } else {
            None
        };
        let forward = args
            .get("--forward")
            .map(|target| address(target))
            .transpose()?;
        let cascade_node = (scenario == "cascade")
            .then(|| args.get("--node").cloned().unwrap_or_else(|| "C".into()));
        InteropServer::new(Handler {
            metadata_node,
            cascade_node,
            errors: scenario == "errors",
            forward,
        })
        .run(incoming)
        .await?;
        return Ok(());
    }
    let address = address(addr)?;
    let pool =
        args.get("--pool")
            .map(String::as_str)
            .unwrap_or(if role == "metadata-batch-client" {
                "mux"
            } else {
                "long"
            });
    let pool_mode = match pool {
        "short" => PoolMode::Short,
        "long" => PoolMode::Long,
        "mux" => PoolMode::Multiplex,
        _ => return Err("invalid pool".into()),
    };
    let config = StreamConfig {
        pool_mode,
        deadline: if role == "cascade-client"
            && args.get("--mode").map(String::as_str) == Some("deadline")
        {
            Some(Duration::from_secs(3))
        } else {
            role.starts_with("metadata-")
                .then_some(Duration::from_secs(10))
        },
        ..Default::default()
    };
    let client = InteropClientBuilder::new("Interop")
        .caller_name(if role == "cascade-client" { "A" } else { "" })
        .address(address)
        .streaming_config(config)
        .build();
    if role == "response-client" {
        let report = response_probe::observe(client.bidi()).await;
        println!("{}", sonic_rs::to_string(&report)?);
        return Ok(());
    }
    if role == "error-client" {
        let mode = args.get("--mode").map(String::as_str).unwrap_or("biz");
        if !matches!(mode, "biz" | "application" | "ok") {
            return Err("invalid error mode".into());
        }
        let identity = args
            .get("--payload")
            .map(String::as_str)
            .unwrap_or("error-case");
        let run = async |kind: &str, id: &str| {
            let code = match kind {
                "biz" => 4123,
                "application" => 6,
                _ => 0,
            };
            let request = sonic_rs::json!({"schemaVersion":1,"id":id,"kind":kind,"code":code,"message":"fixture-terminal","extra":{"detail":"interop-中文"}});
            let value = sonic_rs::to_string(&request)?;
            let first = Message {
                value: value.into(),
                seq: 1,
            };
            let (mut tx, mut rx) = client.bidi().await?.into_split();
            tx.send(&first).await?;
            let response = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await??
                .ok_or("error-status missing echo")?;
            if response.seq != first.seq || response.value != first.value {
                return Err("error-status echo mismatch".into());
            }
            let result = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await?;
            let terminal = error_status::observed(result)?;
            let header = error_status::metadata_strings(&rx.header().await?)?;
            let trailer = error_status::metadata_strings(
                &rx.trailer().ok_or("error-status missing trailer")?,
            )?;
            let response = sonic_rs::json!({"value":response.value,"seq":response.seq});
            Ok::<_, Box<dyn Error + Send + Sync>>((vec![response], terminal, header, trailer))
        };
        let (responses, terminal, header, trailer) = run(mode, identity).await?;
        let (_, probe_terminal, _, _) = run("ok", &format!("{identity}-probe")).await?;
        let probe_ok = probe_terminal.get("kind").as_str() == Some("ok");
        let mut ok = terminal.get("kind").as_str() == Some(mode) && probe_ok;
        if mode == "biz" {
            ok &= terminal.get("code").as_i64() == Some(4123)
                && terminal.get("message").as_str() == Some("fixture-terminal")
                && terminal.get("extra").get("detail").as_str() == Some("interop-中文");
        } else if mode == "application" {
            ok &= terminal.get("code").as_i64() == Some(6)
                && terminal.get("kitex_code").as_i64() == Some(12001)
                && matches!(
                    terminal.get("message").as_str(),
                    Some("fixture-terminal" | "biz error: fixture-terminal")
                );
        }
        let report = sonic_rs::to_string(
            &sonic_rs::json!({"event":"result","api":"errors","id":identity,"mode":mode,"responses":responses,"terminal":terminal,"header":header,"trailer":trailer,"probe_ok":probe_ok,"ok":ok}),
        )?;
        println!("{report}");
        if let Some(path) = args.get("--report") {
            std::fs::write(path, report)?;
        }
        if !ok {
            return Err("error-status observation mismatch".into());
        }
        return Ok(());
    }
    if role == "cascade-client" {
        let mode = args.get("--mode").map(String::as_str).unwrap_or("cancel");
        if !matches!(mode, "cancel" | "deadline") {
            return Err("invalid cascade mode".into());
        }
        let id = args
            .get("--payload")
            .map(String::as_str)
            .unwrap_or("cascade-case");
        let request = sonic_rs::json!({"schemaVersion":1,"id":id,"action":"cancel"});
        let started = Instant::now();
        let (mut tx, mut rx) = client.bidi().await?.into_split();
        tx.send(&Message {
            value: sonic_rs::to_string(&request)?.into(),
            seq: 1,
        })
        .await?;
        let response = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await??
            .ok_or("cascade missing ack")?;
        let response_value: Value = sonic_rs::from_str(&response.value)?;
        if response.seq != 1 || response_value.get("id").as_str() != Some(id) {
            return Err("cascade ack identity mismatch".into());
        }
        if mode == "cancel" {
            tx.context().cancel_local("cascade client canceled");
        }
        let error = match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await? {
            Err(error) => error,
            Ok(_) => return Err("cascade unexpectedly ended without cancellation".into()),
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let code = error.kitex_code();
        let path = match &error {
            StreamError::Canceled { path, .. } => path.clone(),
            _ => Vec::new(),
        };
        drop(tx);
        drop(rx);
        let probe_id = format!("{id}-probe");
        let probe = sonic_rs::json!({"schemaVersion":1,"id":probe_id,"action":"probe"});
        let (mut tx, mut rx) = client.bidi().await?.into_split();
        tx.send(&Message {
            value: sonic_rs::to_string(&probe)?.into(),
            seq: 1,
        })
        .await?;
        tx.close_send().await?;
        let response = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await??
            .ok_or("cascade missing probe response")?;
        let response_value: Value = sonic_rs::from_str(&response.value)?;
        let probe_ok = response.seq == 1
            && response_value.get("id").as_str() == Some(probe_id.as_str())
            && tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await??
                .is_none();
        let ok = code == Some(if mode == "cancel" { 12007 } else { 12015 }) && probe_ok;
        let report = sonic_rs::to_string(
            &sonic_rs::json!({"event":"result","api":"cascade","ok":ok,"mode":mode,"id":id,"ack":true,"code":code,"error":error.to_string(),"ttscp":path,"elapsed_ms":elapsed_ms,"probe_ok":probe_ok}),
        )?;
        println!("{report}");
        if let Some(path) = args.get("--report") {
            std::fs::write(path, report)?;
        }
        if !ok {
            return Err("cascade result mismatch".into());
        }
        return Ok(());
    }
    if role == "metadata-client" || role == "metadata-batch-client" {
        let input = args
            .get("--input")
            .ok_or("metadata client requires --input JSON or file")?;
        let input = if input.trim_start().starts_with(['{', '[']) {
            input.clone()
        } else {
            std::fs::read_to_string(input)?
        };
        let input: Value = sonic_rs::from_str(&input)?;
        let seeds = if role == "metadata-batch-client" {
            let seeds = input.as_array().ok_or("batch seed must be an array")?;
            if seeds.is_empty() || seeds.len() > 32 {
                return Err("batch requires 1..32 seeds".into());
            }
            seeds.iter().cloned().collect::<Vec<_>>()
        } else {
            vec![input]
        };
        let mut results = vec![None; seeds.len()];
        let mut tasks = tokio::task::JoinSet::new();
        for (index, seed) in seeds.into_iter().enumerate() {
            // Clones share this client's mux pool. Each future owns its task-local seed.
            let client = client.clone();
            tasks.spawn(async move {
                let mut info = MetaInfo::default();
                for (key, value) in string_entries(seed.get("persistent"))? { info.set_persistent(key, value); }
                for (key, value) in string_entries(seed.get("transient"))? { info.set_transient(key, value); }
                let request = seed.get("request").ok_or("missing metadata request")?;
                let id = request.get("id").as_str().ok_or("missing string request id")?.to_owned();
                let payload = sonic_rs::to_string(request)?;
                let observations = METAINFO.scope(RefCell::new(info), async {
                    let (mut tx, mut rx) = client.bidi().await?.into_split();
                    tx.send(&Message { value: payload.into(), seq: 1 }).await?;
                    tx.close_send().await?;
                    let response = rx.recv().await?.ok_or("missing metadata response")?;
                    if response.seq != 1 || rx.recv().await?.is_some() { return Err("unexpected metadata response sequence/count".into()); }
                    let result: Value = sonic_rs::from_str(&response.value)?;
                    if result.get("id").as_str() != Some(id.as_str()) { return Err("metadata response identity mismatch".into()); }
                    Ok::<_, Box<dyn Error + Send + Sync>>(result.get("observations").ok_or("missing observations")?.clone())
                }).await?;
                Ok::<_, Box<dyn Error + Send + Sync>>((index, sonic_rs::json!({"event":"result", "ok":true, "api":"metainfo", "id":id, "observations":observations})))
            });
        }
        while let Some(result) = tasks.join_next().await {
            let (index, result) = result??;
            results[index] = Some(result);
        }
        let mut results = results
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or("missing batch result")?;
        let report = if role == "metadata-batch-client" {
            sonic_rs::json!({"event":"result", "ok":true, "api":"metainfo-batch", "results":results})
        } else {
            results.remove(0)
        };
        let report = sonic_rs::to_string(&report)?;
        println!("{report}");
        if let Some(path) = args.get("--report") {
            std::fs::write(path, report)?;
        }
        return Ok(());
    }
    let mode = args.get("--mode").map(String::as_str).unwrap_or("bidi");
    let count = args
        .get("--messages")
        .map(|s| s.parse::<i32>())
        .transpose()?
        .unwrap_or(3);
    let payload = args.get("--payload").map(String::as_str).unwrap_or("hello");
    let mut responses = Vec::new();
    match mode {
        "client" => {
            let mut stream = client.client().await?;
            for seq in 0..count {
                stream
                    .send(&Message {
                        value: payload.to_owned().into(),
                        seq,
                    })
                    .await?;
            }
            responses.push(stream.close_and_recv().await?);
        }
        "server" => {
            let mut stream = client
                .server(Message {
                    value: payload.to_owned().into(),
                    seq: count,
                })
                .await?;
            while let Some(message) = stream.recv().await? {
                responses.push(message);
            }
        }
        "bidi" => {
            let (mut tx, mut rx) = client.bidi().await?.into_split();
            for seq in 0..count {
                tx.send(&Message {
                    value: payload.to_owned().into(),
                    seq,
                })
                .await?;
            }
            tx.close_send().await?;
            while let Some(message) = rx.recv().await? {
                responses.push(message);
            }
        }
        "unary" => {
            responses.push(
                client
                    .unary(Message {
                        value: payload.to_owned().into(),
                        seq: count,
                    })
                    .await?,
            );
        }
        "explicit-unary" => {
            responses.push(
                client
                    .explicit_unary(Message {
                        value: payload.to_owned().into(),
                        seq: count,
                    })
                    .await?,
            );
        }
        _ => return Err("invalid mode".into()),
    }
    let responses = responses
        .into_iter()
        .map(|m| {
            format!(
                "{{\"value\":{},\"seq\":{}}}",
                sonic_rs::to_string(&m.value).unwrap(),
                m.seq
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let report = format!(
        r#"{{"event":"result","ok":true,"api":"typed","pool":{},"mode":{},"responses":[{}],"status":"ok"}}"#,
        sonic_rs::to_string(pool)?,
        sonic_rs::to_string(mode)?,
        responses
    );
    println!("{report}");
    if let Some(path) = args.get("--report") {
        std::fs::write(path, report)?;
    }
    Ok(())
}
