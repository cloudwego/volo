use std::{io, net::SocketAddr};

use socket2::{Domain, Protocol, Socket, Type};
#[cfg(target_family = "unix")]
use tokio::net::UnixStream;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpSocket, TcpStream},
    time::{timeout, Duration},
};

use super::{
    conn::{Conn, OwnedReadHalf, OwnedWriteHalf},
    Address,
};

/// [`MakeTransport`] creates an [`AsyncRead`] and an [`AsyncWrite`] for the given [`Address`].
#[async_trait::async_trait]
pub trait MakeTransport: Clone + Send + Sync + 'static {
    type ReadHalf: AsyncRead + Send + Sync + Unpin + 'static;
    type WriteHalf: AsyncWrite + Send + Sync + Unpin + 'static;

    async fn make_transport(&self, addr: Address) -> io::Result<(Self::ReadHalf, Self::WriteHalf)>;
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

#[async_trait::async_trait]
impl MakeTransport for DefaultMakeTransport {
    type ReadHalf = OwnedReadHalf;

    type WriteHalf = OwnedWriteHalf;

    async fn make_transport(&self, addr: Address) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
        let conn = self.make_connection(addr).await?;
        let (read, write) = conn.stream.into_split();
        Ok((read, write))
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

async fn make_tcp_connection(cfg: &Config, addr: SocketAddr) -> Result<TcpStream, io::Error> {
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

impl DefaultMakeTransport {
    pub async fn make_connection(&self, addr: Address) -> Result<Conn, io::Error> {
        match addr {
            Address::Ip(addr) => {
                let stream = make_tcp_connection(&self.cfg, addr).await?;
                stream.set_nodelay(true)?;
                Ok(Conn::from(stream))
            }
            #[cfg(target_family = "unix")]
            Address::Unix(addr) => UnixStream::connect(addr).await.map(Conn::from),
        }
    }
}

cfg_rustls_or_native_tls! {
    /// A wrapper around [`tokio_rustls::TlsConnector`] and [`tokio_native_tls::TlsConnector`].
    #[derive(Clone)]
    pub enum TlsConnector {
        #[doc(cfg(feature = "rustls"))]
        #[cfg(feature = "rustls")]
        Rustls(tokio_rustls::TlsConnector),
    
        /// This takes an `Arc` because `tokio_native_tls::TlsConnector` does not internally use `Arc`
        #[doc(cfg(feature = "native-tls"))]
        #[cfg(feature = "native-tls")]
        NativeTls(std::sync::Arc<tokio_native_tls::TlsConnector>),
    }

    /// TLS config for client
    #[derive(Clone)]
    pub struct ClientTlsConfig {
        pub domain: String,
        pub connector: TlsConnector,
    }
    
    #[derive(Clone)]
    pub struct DefaultTlsMakeTransport {
        cfg: Config,
        tls_config: ClientTlsConfig,
    }
    
    impl DefaultTlsMakeTransport {
        pub fn new(tls_config: ClientTlsConfig) -> Self {
            Self {
                cfg: Config::default(),
                tls_config,
            }
        }
    
        pub async fn make_connection(&self, addr: Address) -> Result<Conn, io::Error> {
            match addr {
                Address::Ip(addr) => {
                    let tcp = make_tcp_connection(&self.cfg, addr).await?;
    
                    match &self.tls_config.connector {
                        #[cfg(feature = "rustls")]
                        TlsConnector::Rustls(connector) => {
                            let domain = librustls::ServerName::try_from(&self.tls_config.domain[..])
                                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                            connector
                                .connect(domain, tcp)
                                .await
                                .map(tokio_rustls::TlsStream::Client)
                                .map(Conn::from)
                        }
                        #[cfg(feature = "native-tls")]
                        TlsConnector::NativeTls(connector) => {
                            let tcp = make_tcp_connection(&self.cfg, addr).await?;
                            connector
                                .connect(&self.tls_config.domain[..], tcp)
                                .await
                                .map(Conn::from)
                                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
                        }
                    }
                }
                #[cfg(target_family = "unix")]
                Address::Unix(addr) => UnixStream::connect(addr).await.map(Conn::from),
            }
        }
    }
    
    #[async_trait::async_trait]
    impl MakeTransport for DefaultTlsMakeTransport {
        type ReadHalf = OwnedReadHalf;
    
        type WriteHalf = OwnedWriteHalf;
    
        async fn make_transport(&self, addr: Address) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
            let conn = self.make_connection(addr).await?;
            let (read, write) = conn.stream.into_split();
            Ok((read, write))
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
}
