use std::io;

use socket2::{Domain, Protocol, Socket, Type};
#[cfg(target_family = "unix")]
use tokio::net::UnixStream;
use tokio::{
    net::{TcpSocket, TcpStream},
    time::{timeout, Duration},
};

use super::{conn::Conn, Address};

#[derive(Default, Debug, Clone)]
pub struct MakeConnection {
    cfg: Option<Config>,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct Config {
    connect_timeout: Option<Duration>,
    read_write_timeout: Option<Duration>,
}

impl Config {
    pub fn new(connect_timeout: Option<Duration>, read_write_timeout: Option<Duration>) -> Self {
        Self {
            connect_timeout,
            read_write_timeout,
        }
    }
}

impl MakeConnection {
    pub fn new(cfg: Option<Config>) -> Self {
        Self { cfg }
    }
}

impl MakeConnection {
    pub async fn make_connection(&self, addr: Address) -> Result<Conn, io::Error> {
        match addr {
            Address::Ip(addr) => {
                let stream = if let Some(cfg) = self.cfg {
                    let domain = Domain::for_address(addr);
                    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
                    socket.set_nonblocking(true)?;
                    socket.set_read_timeout(cfg.read_write_timeout)?;
                    socket.set_write_timeout(cfg.read_write_timeout)?;

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
                        timeout(conn_timeout, connect).await??
                    } else {
                        connect.await?
                    }
                } else {
                    TcpStream::connect(addr).await?
                };
                stream.set_nodelay(true)?;
                Ok(Conn::from(stream))
            }
            #[cfg(target_family = "unix")]
            Address::Unix(addr) => UnixStream::connect(addr).await.map(Conn::from),
        }
    }
}
