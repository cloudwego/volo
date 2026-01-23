use std::{
    fmt,
    future::Future,
    io,
    task::{Context, Poll},
};

use futures::Stream;
use pin_project::pin_project;
use tokio::net::TcpListener;
#[cfg(target_family = "unix")]
use tokio::net::UnixListener;
#[cfg(target_family = "unix")]
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::{StreamExt, wrappers::TcpListenerStream};

use super::{Address, conn::Conn};

#[pin_project(project = IncomingProj)]
#[derive(Debug)]
pub enum DefaultIncoming {
    Tcp(#[pin] TcpListenerStream),
    #[cfg(target_family = "unix")]
    Unix(#[pin] UnixListenerStream),
    #[cfg(feature = "shmipc")]
    Shmipc(#[pin] super::shmipc::conn::ListenerStream),
}

impl MakeIncoming for DefaultIncoming {
    type Incoming = DefaultIncoming;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        Ok(self)
    }
}

impl From<TcpListener> for DefaultIncoming {
    fn from(l: TcpListener) -> Self {
        DefaultIncoming::Tcp(TcpListenerStream::new(l))
    }
}

#[cfg(target_family = "unix")]
impl From<UnixListener> for DefaultIncoming {
    fn from(l: UnixListener) -> Self {
        DefaultIncoming::Unix(UnixListenerStream::new(l))
    }
}

#[cfg(feature = "shmipc")]
impl From<super::shmipc::Listener> for DefaultIncoming {
    fn from(value: super::shmipc::Listener) -> Self {
        DefaultIncoming::Shmipc(super::shmipc::conn::ListenerStream::new(value))
    }
}

pub trait Incoming: fmt::Debug + Send + 'static {
    fn accept(&mut self) -> impl Future<Output = io::Result<Option<Conn>>> + Send;
}

impl Incoming for DefaultIncoming {
    async fn accept(&mut self) -> io::Result<Option<Conn>> {
        if let Some(conn) = self.try_next().await? {
            tracing::trace!("[VOLO] recv a connection from: {:?}", conn.info.peer_addr);
            Ok(Some(conn))
        } else {
            Ok(None)
        }
    }
}

pub trait MakeIncoming {
    type Incoming: Incoming;

    fn make_incoming(self) -> impl Future<Output = io::Result<Self::Incoming>> + Send;
}

#[cfg(target_family = "unix")]
impl MakeIncoming for Address {
    type Incoming = DefaultIncoming;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        match self {
            Address::Ip(addr) => {
                let listener = unix_helper::create_tcp_listener_with_max_backlog(addr).await;
                TcpListener::from_std(listener?).map(DefaultIncoming::from)
            }
            Address::Unix(addr) => {
                let listener = unix_helper::create_unix_listener_with_max_backlog(
                    addr.as_pathname().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::AddrNotAvailable,
                            "cannot create unnamed socket",
                        )
                    })?,
                )
                .await;
                UnixListener::from_std(listener?).map(DefaultIncoming::from)
            }
            #[cfg(feature = "shmipc")]
            Address::Shmipc(addr) => super::shmipc::conn::Listener::listen(addr, None)
                .await
                .map(DefaultIncoming::from),
        }
    }
}

#[cfg(not(target_family = "unix"))]
impl MakeIncoming for Address {
    type Incoming = DefaultIncoming;

    async fn make_incoming(self) -> io::Result<Self::Incoming> {
        match self {
            Address::Ip(addr) => TcpListener::bind(addr).await.map(DefaultIncoming::from),
            #[cfg(target_family = "unix")]
            Address::Unix(addr) => UnixListener::bind(addr).map(DefaultIncoming::from),
        }
    }
}

impl Stream for DefaultIncoming {
    type Item = io::Result<Conn>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            IncomingProj::Tcp(s) => s.poll_next(cx).map_ok(Conn::from),
            #[cfg(target_family = "unix")]
            IncomingProj::Unix(s) => s.poll_next(cx).map_ok(Conn::from),
            #[cfg(feature = "shmipc")]
            IncomingProj::Shmipc(s) => s.poll_next(cx).map_ok(Conn::from),
        }
    }
}

#[cfg(target_family = "unix")]
mod unix_helper {

    #[cfg(target_os = "linux")]
    use std::{
        fs::File,
        io::{BufRead, BufReader},
    };
    use std::{
        net::{SocketAddr, TcpListener},
        os::{
            fd::{AsRawFd, FromRawFd, IntoRawFd},
            unix::net::UnixListener,
        },
        path::Path,
    };

    use socket2::{Domain, Protocol, Socket, Type};

    use crate::hotrestart::DEFAULT_HOT_RESTART;

    /// Returns major and minor kernel version numbers, parsed from
    /// the nix::sys::utsname's release field, or 0, 0 if the version can't be obtained
    /// or parsed.
    ///
    /// Currently only implemented for Linux.
    #[cfg(target_os = "linux")]
    pub fn kernel_version() -> (i32, i32) {
        let uname_info = if let Ok(uname_info) = nix::sys::utsname::uname() {
            uname_info
        } else {
            return (0, 0);
        };
        let release = if let Some(release) = uname_info.release().to_str() {
            release
        } else {
            return (0, 0);
        };

        let mut values = [0, 0];
        let mut value = 0;
        let mut vi = 0;

        for &c in release.as_bytes() {
            if c.is_ascii_digit() {
                value = (value * 10) + ((c - b'0') as i32);
            } else {
                values[vi] = value;
                vi += 1;
                if vi >= values.len() {
                    break;
                }
                value = 0;
            }
        }

        (values[0], values[1])
    }

    #[cfg(target_os = "linux")]
    pub fn split_at_bytes(s: &str, t: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut last = 0;
        for (i, c) in s.char_indices() {
            if t.contains(c) {
                if last < i {
                    result.push(s[last..i].to_string());
                }
                last = i + c.len_utf8();
            }
        }
        if last < s.len() {
            result.push(s[last..].to_string());
        }
        result
    }

    #[cfg(target_os = "linux")]
    pub fn get_fields(s: &str) -> Vec<String> {
        split_at_bytes(s, " \r\t\n")
    }

    /// Linux stores the backlog as:
    ///
    ///   - uint16 in kernel version < 4.1,
    ///   - uint32 in kernel version >= 4.1
    ///
    /// Truncate number to avoid wrapping.
    #[cfg(target_os = "linux")]
    pub fn max_ack_backlog(n: i32) -> i32 {
        let (major, minor) = kernel_version();
        let size = if major > 4 || (major == 4 && minor >= 1) {
            32
        } else {
            16
        };

        let max = (1 << size) - 1;
        if n > max { max } else { n }
    }

    #[cfg(target_os = "linux")]
    pub fn max_listener_backlog() -> i32 {
        let file = File::open("/proc/sys/net/core/somaxconn");
        let file = match file {
            Ok(file) => file,
            Err(_) => return libc::SOMAXCONN,
        };
        let mut reader = BufReader::new(file);
        let mut line = String::new();

        let read_result = reader.read_line(&mut line);
        if read_result.is_err() || line.is_empty() {
            return libc::SOMAXCONN;
        }
        let fields = get_fields(&line);
        if let Ok(n) = fields[0].parse() {
            if n > ((1 << 16) - 1) {
                max_ack_backlog(n)
            } else {
                n
            }
        } else {
            libc::SOMAXCONN
        }
    }

    pub async fn create_tcp_listener_with_max_backlog(
        addr: SocketAddr,
    ) -> std::io::Result<TcpListener> {
        if let Ok(Some(raw_fd)) = DEFAULT_HOT_RESTART
            .dup_parent_listener_sock(addr.to_string())
            .await
        {
            DEFAULT_HOT_RESTART.register_listener_fd(addr.to_string(), raw_fd);
            let socket = unsafe { Socket::from_raw_fd(raw_fd) };
            return Ok(socket.into());
        }

        let domain = if addr.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };

        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        socket.set_reuse_port(true)?;
        socket.set_cloexec(true)?;

        socket.bind(&socket2::SockAddr::from(addr))?;

        #[cfg(target_os = "linux")]
        let backlog = max_listener_backlog();
        #[cfg(not(target_os = "linux"))]
        let backlog = libc::SOMAXCONN;
        socket.listen(backlog)?;

        DEFAULT_HOT_RESTART.register_listener_fd(addr.to_string(), socket.as_raw_fd());
        Ok(socket.into())
    }

    pub async fn create_unix_listener_with_max_backlog<P: AsRef<Path>>(
        path: P,
    ) -> std::io::Result<UnixListener> {
        if let Some(path_str) = path.as_ref().to_str() {
            if let Ok(Some(raw_fd)) = DEFAULT_HOT_RESTART
                .dup_parent_listener_sock(path_str.to_string())
                .await
            {
                DEFAULT_HOT_RESTART.register_listener_fd(path_str.to_string(), raw_fd);
                let unix_listener = unsafe { UnixListener::from_raw_fd(raw_fd) };
                return Ok(unix_listener);
            }

            let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
            socket.set_nonblocking(true)?;
            socket.set_cloexec(true)?;

            let path = path.as_ref();
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            socket.bind(&socket2::SockAddr::unix(path)?)?;
            #[cfg(target_os = "linux")]
            let backlog = max_listener_backlog();
            #[cfg(not(target_os = "linux"))]
            let backlog = libc::SOMAXCONN;
            socket.listen(backlog)?;

            // Convert the socket into a UnixListener
            let raw_fd = socket.into_raw_fd();
            DEFAULT_HOT_RESTART.register_listener_fd(path_str.to_string(), raw_fd);
            let unix_listener = unsafe { UnixListener::from_raw_fd(raw_fd) };

            Ok(unix_listener)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid path",
            ))
        }
    }
}
