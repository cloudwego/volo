use std::{future::Future, io, net::SocketAddr};

use motore::{make::MakeConnection, service::UnaryService};
use socket2::{Domain, Protocol, Socket, Type};
#[cfg(target_family = "unix")]
use tokio::net::UnixStream;
use tokio::{
    net::{TcpSocket, TcpStream},
    time::{timeout, Duration},
};

use super::{
    conn::{Conn, ConnExt},
    Address,
};

/// [`MakeTransport`] creates a [`Conn`] for the given [`Address`].
pub trait MakeTransport: Clone + Send + Sync + 'static {
    type Conn: ConnExt;

    fn make_transport(&self, addr: Address) -> impl Future<Output = io::Result<Self::Conn>> + Send;
    fn set_connect_timeout(&mut self, timeout: Option<Duration>);
    fn set_read_timeout(&mut self, timeout: Option<Duration>);
    fn set_write_timeout(&mut self, timeout: Option<Duration>);
}

#[derive(Default, Debug, Clone, Copy)]
pub struct DefaultMakeTransport {
    cfg: Config,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct Config {
    pub connect_timeout: Option<Duration>,
    pub read_timeout: Option<Duration>,
    pub write_timeout: Option<Duration>,
}

impl Config {
    pub fn new(
        connect_timeout: Option<Duration>,
        read_timeout: Option<Duration>,
        write_timeout: Option<Duration>,
    ) -> Self {
        Self {
            connect_timeout,
            read_timeout,
            write_timeout,
        }
    }

    pub fn with_connect_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn with_read_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.read_timeout = timeout;
        self
    }

    pub fn with_write_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.write_timeout = timeout;
        self
    }
}

impl DefaultMakeTransport {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MakeTransport for DefaultMakeTransport {
    type Conn = Conn;

    async fn make_transport(&self, addr: Address) -> io::Result<Conn> {
        self.make_connection(addr).await
    }

    fn set_connect_timeout(&mut self, timeout: Option<Duration>) {
        self.cfg = self.cfg.with_connect_timeout(timeout);
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.cfg = self.cfg.with_read_timeout(timeout);
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) {
        self.cfg = self.cfg.with_write_timeout(timeout);
    }
}

pub(super) async fn make_tcp_connection(
    cfg: &Config,
    addr: SocketAddr,
) -> Result<TcpStream, io::Error> {
    let domain = Domain::for_address(addr);
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_nonblocking(true)?;
    socket.set_read_timeout(cfg.read_timeout)?;
    socket.set_write_timeout(cfg.write_timeout)?;

    #[cfg(unix)]
    let socket = unsafe {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        TcpSocket::from_raw_fd(socket.into_raw_fd())
    };
    #[cfg(windows)]
    let socket = unsafe {
        use std::os::windows::io::{FromRawSocket, IntoRawSocket};
        TcpSocket::from_raw_socket(socket.into_raw_socket())
    };

    let connect = socket.connect(addr);

    if let Some(conn_timeout) = cfg.connect_timeout {
        timeout(conn_timeout, connect).await?
    } else {
        connect.await
    }
}

impl UnaryService<Address> for DefaultMakeTransport {
    type Response = Conn;
    type Error = io::Error;

    async fn call(&self, addr: Address) -> Result<Self::Response, Self::Error> {
        match addr {
            Address::Ip(addr) => {
                let stream = make_tcp_connection(&self.cfg, addr).await?;
                stream.set_nodelay(true)?;
                Ok(Conn::from(stream))
            }
            #[cfg(target_family = "unix")]
            Address::Unix(addr) => UnixStream::connect(addr.as_pathname().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "cannot connect to unnamed socket",
                )
            })?)
            .await
            .map(Conn::from),
        }
    }
}
