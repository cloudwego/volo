use std::{
    env,
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, ServerOptions},
    task::JoinHandle,
    time::sleep,
};

const PIPE_NAME: &str = r"\\.\pipe\volo_named_pipe_stress_example";
const DEFAULT_CONNECTIONS: usize = 8;
const DEFAULT_ITERATIONS: usize = 1_000;
const DEFAULT_PAYLOAD_LEN: usize = 1024;
const DEFAULT_FLUSH_EVERY: usize = 200;
const DEFAULT_WRITE_PAUSE_US: u64 = 0;

#[derive(Debug, Clone)]
struct StressConfig {
    connections: usize,
    iterations: usize,
    payload_len: usize,
    flush_every: usize,
    write_pause_us: u64,
}

impl StressConfig {
    fn from_env() -> io::Result<Self> {
        Ok(Self {
            connections: read_env_usize("VOLO_NP_STRESS_CONNECTIONS", DEFAULT_CONNECTIONS)?,
            iterations: read_env_usize("VOLO_NP_STRESS_ITERATIONS", DEFAULT_ITERATIONS)?,
            payload_len: read_env_usize("VOLO_NP_STRESS_PAYLOAD_LEN", DEFAULT_PAYLOAD_LEN)?,
            flush_every: read_env_usize("VOLO_NP_STRESS_FLUSH_EVERY", DEFAULT_FLUSH_EVERY)?,
            write_pause_us: read_env_u64("VOLO_NP_STRESS_WRITE_PAUSE_US", DEFAULT_WRITE_PAUSE_US)?,
        })
    }

    fn validate(&self) -> io::Result<()> {
        if self.connections == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VOLO_NP_STRESS_CONNECTIONS must be greater than 0",
            ));
        }
        if self.iterations == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VOLO_NP_STRESS_ITERATIONS must be greater than 0",
            ));
        }
        if self.payload_len < 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VOLO_NP_STRESS_PAYLOAD_LEN must be at least 16",
            ));
        }
        if self.flush_every == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VOLO_NP_STRESS_FLUSH_EVERY must be greater than 0",
            ));
        }
        Ok(())
    }
}

#[volo::main]
async fn main() -> io::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Arc::new(StressConfig::from_env()?);
    cfg.validate()?;

    println!("starting named pipe stress test with config: {cfg:?}");

    let started_at = Instant::now();
    let server = tokio::spawn(run_server_pool(cfg.clone()));
    let client = tokio::spawn(run_client_pool(cfg.clone()));

    let (server_res, client_res) = tokio::try_join!(join_result(server), join_result(client))?;
    server_res?;
    client_res?;

    println!(
        "named pipe full-duplex stress test passed: {} connections, {} messages each side, {} bytes each message, elapsed {:?}",
        cfg.connections,
        cfg.iterations,
        cfg.payload_len,
        started_at.elapsed(),
    );
    Ok(())
}

async fn join_result(handle: JoinHandle<io::Result<()>>) -> io::Result<io::Result<()>> {
    handle
        .await
        .map_err(|err| io::Error::new(io::ErrorKind::Other, format!("task join error: {err}")))
}

async fn run_server_pool(cfg: Arc<StressConfig>) -> io::Result<()> {
    let mut tasks = Vec::with_capacity(cfg.connections);
    for conn_id in 0..cfg.connections {
        let cfg = cfg.clone();
        tasks.push(tokio::spawn(async move { run_server_connection(cfg, conn_id).await }));
    }

    for task in tasks {
        join_result(task).await??;
    }
    Ok(())
}

async fn run_client_pool(cfg: Arc<StressConfig>) -> io::Result<()> {
    // Give the server a short head start to create the first pipe instance.
    sleep(Duration::from_millis(50)).await;

    let mut tasks = Vec::with_capacity(cfg.connections);
    for conn_id in 0..cfg.connections {
        let cfg = cfg.clone();
        tasks.push(tokio::spawn(async move { run_client_connection(cfg, conn_id).await }));
    }

    for task in tasks {
        join_result(task).await??;
    }
    Ok(())
}

async fn run_server_connection(cfg: Arc<StressConfig>, conn_id: usize) -> io::Result<()> {
    let server = ServerOptions::new()
        .first_pipe_instance(conn_id == 0)
        .create(PIPE_NAME)?;
    server.connect().await?;

    run_duplex(
        volo::net::conn::ConnStream::from(server),
        cfg,
        conn_id,
        b'S',
        b'C',
    )
    .await
}

async fn run_client_connection(cfg: Arc<StressConfig>, conn_id: usize) -> io::Result<()> {
    let client = loop {
        match ClientOptions::new().open(PIPE_NAME) {
            Ok(client) => break client,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::NotFound
                ) =>
            {
                sleep(Duration::from_millis(10)).await;
            }
            Err(err) => return Err(err),
        }
    };

    run_duplex(
        volo::net::conn::ConnStream::from(client),
        cfg,
        conn_id,
        b'C',
        b'S',
    )
    .await
}

async fn run_duplex(
    stream: volo::net::conn::ConnStream,
    cfg: Arc<StressConfig>,
    conn_id: usize,
    write_marker: u8,
    read_marker: u8,
) -> io::Result<()> {
    let (mut read_half, mut write_half) = stream.into_split();

    let writer_cfg = cfg.clone();
    let writer = tokio::spawn(async move {
        write_frames(&mut write_half, &writer_cfg, conn_id, write_marker).await
    });
    let reader = tokio::spawn(async move {
        read_frames(&mut read_half, &cfg, conn_id, read_marker).await
    });

    join_result(writer).await??;
    join_result(reader).await??;
    Ok(())
}

async fn write_frames<W>(
    writer: &mut W,
    cfg: &StressConfig,
    conn_id: usize,
    marker: u8,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    for seq in 0..cfg.iterations {
        let payload = make_payload(cfg.payload_len, marker, conn_id, seq);
        writer.write_u32_le(payload.len() as u32).await?;
        writer.write_all(&payload).await?;
        if (seq + 1) % cfg.flush_every == 0 {
            writer.flush().await?;
        }
        if cfg.write_pause_us > 0 {
            sleep(Duration::from_micros(cfg.write_pause_us)).await;
        }
    }
    writer.flush().await
}

async fn read_frames<R>(
    reader: &mut R,
    cfg: &StressConfig,
    expected_conn_id: usize,
    expected_marker: u8,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    for seq in 0..cfg.iterations {
        let len = reader.read_u32_le().await? as usize;
        if len != cfg.payload_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unexpected payload length on connection {expected_conn_id}: expected {}, got {len}",
                    cfg.payload_len
                ),
            ));
        }

        let mut buf = vec![0_u8; len];
        reader.read_exact(&mut buf).await?;
        validate_payload(&buf, expected_marker, expected_conn_id, seq)?;
    }
    Ok(())
}

fn make_payload(payload_len: usize, marker: u8, conn_id: usize, seq: usize) -> Vec<u8> {
    let mut payload = vec![marker; payload_len];
    payload[..8].copy_from_slice(&(conn_id as u64).to_le_bytes());
    payload[8..16].copy_from_slice(&(seq as u64).to_le_bytes());
    payload
}

fn validate_payload(
    buf: &[u8],
    expected_marker: u8,
    expected_conn_id: usize,
    expected_seq: usize,
) -> io::Result<()> {
    if buf.len() < 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("payload too short: {}", buf.len()),
        ));
    }

    let conn_id =
        u64::from_le_bytes(buf[..8].try_into().expect("payload conn id length")) as usize;
    if conn_id != expected_conn_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected connection id: expected {expected_conn_id}, got {conn_id}"),
        ));
    }

    let seq = u64::from_le_bytes(buf[8..16].try_into().expect("payload seq length")) as usize;
    if seq != expected_seq {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unexpected sequence on connection {expected_conn_id}: expected {expected_seq}, got {seq}"
            ),
        ));
    }

    if buf[16..].iter().any(|byte| *byte != expected_marker) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "payload marker mismatch on connection {expected_conn_id} for seq {expected_seq}"
            ),
        ));
    }

    Ok(())
}

fn read_env_usize(key: &str, default: usize) -> io::Result<usize> {
    match env::var(key) {
        Ok(value) => value.parse::<usize>().map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid {key} value `{value}`: {err}"),
            )
        }),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("failed to read {key}: {err}"),
        )),
    }
}

fn read_env_u64(key: &str, default: u64) -> io::Result<u64> {
    match env::var(key) {
        Ok(value) => value.parse::<u64>().map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid {key} value `{value}`: {err}"),
            )
        }),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("failed to read {key}: {err}"),
        )),
    }
}
