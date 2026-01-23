#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
use std::{collections::HashMap, fmt, hash::Hash, io, sync::LazyLock};

use motore::service::UnaryService;
use shmipc::session::SessionManager;
use tokio::sync::RwLock;

pub(crate) static SESSION_MANAGERS: LazyLock<RwLock<HashMap<Address, SessionManager<Connector>>>> =
    LazyLock::new(Default::default);

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

        {
            let read = SESSION_MANAGERS.read().await;
            if let Some(sm) = read.get(&addr) {
                return sm.get_stream().map(super::Stream::new).map_err(Into::into);
            }
        }

        let config = super::config::session_manager_config();
        tracing::debug!("ShmipcMakeTransport: config: {config:?}");
        let sm = SessionManager::new(config, Connector, addr.clone())
            .await
            .map_err(Into::<io::Error>::into)?;
        let ret = sm.get_stream().map(super::Stream::new).map_err(Into::into);
        SESSION_MANAGERS.write().await.insert(addr, sm);

        ret
    }
}
