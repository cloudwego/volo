#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    hash::Hash,
    io,
    sync::{Arc, LazyLock},
};

use motore::service::UnaryService;
use shmipc::session::SessionManager;
use tokio::sync::{OnceCell, RwLock};

type SharedInitCell<V, E> = Arc<OnceCell<Result<Arc<V>, Arc<E>>>>;
type SessionManagerCell = SharedInitCell<SessionManager<Connector>, io::Error>;

pub(crate) static SESSION_MANAGERS: LazyLock<RwLock<HashMap<Address, SessionManagerCell>>> =
    LazyLock::new(Default::default);

async fn get_or_insert_cell<K, V>(
    cells: &RwLock<HashMap<K, Arc<OnceCell<V>>>>,
    key: K,
) -> Arc<OnceCell<V>>
where
    K: Eq + Hash,
{
    {
        let read = cells.read().await;
        if let Some(cell) = read.get(&key) {
            return Arc::clone(cell);
        }
    }

    let mut write = cells.write().await;
    Arc::clone(
        write
            .entry(key)
            .or_insert_with(|| Arc::new(OnceCell::new())),
    )
}

async fn remove_cell_if_same<K, V>(
    cells: &RwLock<HashMap<K, Arc<OnceCell<V>>>>,
    key: &K,
    expected: &Arc<OnceCell<V>>,
) where
    K: Eq + Hash,
{
    let mut write = cells.write().await;
    let is_same = write
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, expected));
    if is_same {
        write.remove(key);
    }
}

async fn get_or_try_init_shared<K, V, E, F, Fut>(
    cells: &RwLock<HashMap<K, SharedInitCell<V, E>>>,
    key: K,
    init: F,
) -> Result<Arc<V>, Arc<E>>
where
    K: Eq + Hash + Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<V, E>>,
{
    let cell = get_or_insert_cell(cells, key.clone()).await;
    let result = cell
        .get_or_init(|| async { init().await.map(Arc::new).map_err(Arc::new) })
        .await
        .clone();

    // Keep a failed result in this cell long enough for callers that joined this initialization
    // attempt to observe it, but detach the cell from the address map so a later call can retry.
    if result.is_err() {
        remove_cell_if_same(cells, &key, &cell).await;
    }

    result
}

#[derive(Clone, Debug)]
pub enum Address {
    // The address must be loopback addr
    Tcp(std::net::SocketAddr),
    #[cfg(target_family = "unix")]
    Unix(std::os::unix::net::SocketAddr),
    Client(usize, u32),
}

impl From<std::net::SocketAddr> for Address {
    fn from(value: std::net::SocketAddr) -> Self {
        Self::Tcp(value)
    }
}

#[cfg(target_family = "unix")]
impl From<std::os::unix::net::SocketAddr> for Address {
    fn from(value: std::os::unix::net::SocketAddr) -> Self {
        Self::Unix(value)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{addr}"),
            #[cfg(target_family = "unix")]
            Self::Unix(addr) => {
                #[cfg(target_os = "linux")]
                if let Some(abs_name) = addr.as_abstract_name() {
                    return write!(f, "{}", abs_name.escape_ascii());
                }
                if let Some(pathname) = addr.as_pathname() {
                    write!(f, "{}", pathname.to_string_lossy())
                } else {
                    f.write_str("(unnamed)")
                }
            }
            Self::Client(session, stream) => write!(f, "session {session}, stream {stream}"),
        }
    }
}

impl PartialEq for Address {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Tcp(self_ip), Self::Tcp(other_ip)) => self_ip == other_ip,
            #[cfg(target_family = "unix")]
            (Self::Unix(self_uds), Self::Unix(other_uds)) => {
                match (self_uds.as_pathname(), other_uds.as_pathname()) {
                    (Some(self_pathname), Some(other_pathname)) => self_pathname == other_pathname,
                    (None, None) => {
                        // Both uds are unnamed, so they cannot be compared.
                        //
                        // We noticed that the `PartialEq`, `Eq` and `Hash` are only used for load
                        // balance, and load balace can only be used for TCP connection.  So we can
                        // treat the unnamed uds as the same.
                        true
                    }
                    // named and unnamed must be different
                    _ => false,
                }
            }
            (
                Self::Client(self_session, self_stream),
                Self::Client(other_session, other_stream),
            ) => self_session == other_session && self_stream == other_stream,
            #[cfg(target_family = "unix")]
            _ => false,
        }
    }
}

impl Eq for Address {}

impl Hash for Address {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Tcp(ip) => {
                state.write_u8(0);
                Hash::hash(ip, state);
            }
            #[cfg(target_family = "unix")]
            Self::Unix(uds) => {
                #[cfg(target_os = "linux")]
                if let Some(abs_name) = uds.as_abstract_name() {
                    state.write_u8(1);
                    Hash::hash(abs_name, state);
                    return;
                }
                if let Some(pathname) = uds.as_pathname() {
                    state.write_u8(2);
                    Hash::hash(pathname, state);
                } else {
                    state.write_u8(3);
                }
            }
            Self::Client(session_id, stream_id) => {
                state.write_u8(4);
                state.write_usize(*session_id);
                state.write_u32(*stream_id);
            }
        }
    }
}

impl std::os::fd::AsRawFd for crate::net::conn::Conn {
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        match &self.stream {
            crate::net::conn::ConnStream::Tcp(addr) => addr.as_raw_fd(),
            #[cfg(target_family = "unix")]
            crate::net::conn::ConnStream::Unix(addr) => addr.as_raw_fd(),
            _ => panic!("only tcp and unix conn have raw fd"),
        }
    }
}

impl shmipc::transport::TransportStream for crate::net::conn::Conn {
    type ReadHalf = crate::net::conn::OwnedReadHalf;
    type WriteHalf = crate::net::conn::OwnedWriteHalf;

    fn into_split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        match &self.stream {
            crate::net::conn::ConnStream::Tcp(_) => {}
            crate::net::conn::ConnStream::Unix(_) => {}
            _ => panic!("only tcp and unix conn can be used as backend of shmipc"),
        }

        self.stream.into_split()
    }
}

pub(crate) struct Connector;

impl shmipc::transport::TransportConnect for Connector {
    type Stream = crate::net::conn::Conn;
    type Address = Address;

    async fn connect(&self, addr: Self::Address) -> io::Result<Self::Stream> {
        match &addr {
            Address::Tcp(addr) => {
                crate::net::dial::make_tcp_connection(&Default::default(), addr.to_owned())
                    .await
                    .map(crate::net::conn::Conn::from)
            }
            #[cfg(target_family = "unix")]
            Address::Unix(addr) => {
                let Some(path) = addr.as_pathname() else {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        "cannot connect to unnamed socket",
                    ));
                };
                tokio::net::UnixStream::connect(path)
                    .await
                    .map(crate::net::conn::Conn::from)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only tcp and unix address can be used as backend of shmipc",
            )),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ShmipcMakeTransport;

impl ShmipcMakeTransport {
    pub const fn new() -> Self {
        Self
    }
}

impl UnaryService<Address> for ShmipcMakeTransport {
    type Response = super::Stream;
    type Error = io::Error;

    async fn call(&self, addr: Address) -> Result<Self::Response, Self::Error> {
        if matches!(addr, Address::Client(_, _)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "client address cannot be connected",
            ));
        }

        let sm = get_or_try_init_shared(&SESSION_MANAGERS, addr.clone(), || async move {
            let config = super::config::session_manager_config();
            tracing::debug!("ShmipcMakeTransport: config: {config:?}");
            SessionManager::new(config, Connector, addr)
                .await
                .map_err(Into::<io::Error>::into)
        })
        .await
        .map_err(|err| io::Error::new(err.kind(), err.to_string()))?;

        sm.get_stream().map(super::Stream::new).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Barrier;

    use super::*;

    #[tokio::test]
    async fn same_key_initialization_is_singleflight() {
        const CONCURRENCY: usize = 32;

        let cells = Arc::new(RwLock::new(HashMap::<usize, Arc<OnceCell<usize>>>::new()));
        let barrier = Arc::new(Barrier::new(CONCURRENCY));
        let initializations = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::with_capacity(CONCURRENCY);

        for _ in 0..CONCURRENCY {
            let cells = Arc::clone(&cells);
            let barrier = Arc::clone(&barrier);
            let initializations = Arc::clone(&initializations);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let cell = get_or_insert_cell(&cells, 1).await;
                *cell
                    .get_or_try_init(|| async {
                        initializations.fetch_add(1, Ordering::Relaxed);
                        tokio::task::yield_now().await;
                        Ok::<_, ()>(42)
                    })
                    .await
                    .unwrap()
            }));
        }

        for task in tasks {
            assert_eq!(task.await.unwrap(), 42);
        }
        assert_eq!(initializations.load(Ordering::Relaxed), 1);
        assert_eq!(cells.read().await.len(), 1);
    }

    #[tokio::test]
    async fn same_key_failed_initialization_is_shared_then_retried() {
        const CONCURRENCY: usize = 32;

        type TestCell = SharedInitCell<usize, &'static str>;

        let cells = Arc::new(RwLock::new(HashMap::<usize, TestCell>::new()));
        let barrier = Arc::new(Barrier::new(CONCURRENCY));
        let initializations = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::with_capacity(CONCURRENCY);

        for _ in 0..CONCURRENCY {
            let cells = Arc::clone(&cells);
            let barrier = Arc::clone(&barrier);
            let initializations = Arc::clone(&initializations);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                get_or_try_init_shared(&cells, 1, || {
                    let cells = Arc::clone(&cells);
                    let initializations = Arc::clone(&initializations);
                    async move {
                        initializations.fetch_add(1, Ordering::Relaxed);

                        // Do not fail until every concurrent caller holds this same cell. This
                        // makes the waiter-sharing assertion deterministic instead of scheduler
                        // dependent.
                        loop {
                            let strong_count = {
                                let read = cells.read().await;
                                Arc::strong_count(read.get(&1).expect("cell must be registered"))
                            };
                            if strong_count > CONCURRENCY {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }

                        Err("injected initialization failure")
                    }
                })
                .await
                .unwrap_err()
            }));
        }

        let mut errors = Vec::with_capacity(CONCURRENCY);
        for task in tasks {
            errors.push(task.await.unwrap());
        }

        assert_eq!(initializations.load(Ordering::Relaxed), 1);
        assert!(
            errors.iter().all(|error| Arc::ptr_eq(error, &errors[0])),
            "all concurrent waiters must observe the same failed attempt"
        );
        assert!(
            cells.read().await.is_empty(),
            "a failed cell must be detached so a later request can retry"
        );

        let value = get_or_try_init_shared(&cells, 1, || async {
            initializations.fetch_add(1, Ordering::Relaxed);
            Ok::<_, &'static str>(42)
        })
        .await
        .unwrap();

        assert_eq!(*value, 42);
        assert_eq!(initializations.load(Ordering::Relaxed), 2);
        assert_eq!(cells.read().await.len(), 1);
    }
}
