use std::{io, net::TcpStream as StdTcpStream};

use tokio::{
    net::{TcpStream, UnixStream},
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
                    let stream = if let Some(conn_timeout) = cfg.connect_timeout {
                        StdTcpStream::connect_timeout(&addr, conn_timeout)?
                    } else {
                        StdTcpStream::connect(&addr)?
                    };
                    stream.set_nonblocking(true)?;
                    stream.set_read_timeout(cfg.read_write_timeout)?;
                    stream.set_write_timeout(cfg.read_write_timeout)?;
                    let stream = TcpStream::from_std(stream)?;
                    if let Some(conn_timeout) = cfg.connect_timeout {
                        timeout(conn_timeout, stream.writable()).await??;
                    }
                    stream
                } else {
                    TcpStream::connect(addr).await?
                };
                stream.set_nodelay(true)?;
                Ok(Conn::from(stream))
            }
            Address::Unix(addr) => UnixStream::connect(addr).await.map(Conn::from),
        }
    }
}
