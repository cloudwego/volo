use std::{future::Future, io, net::SocketAddr};

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
pub trait MakeTransport: Clone + Send + Sync + 'static {
    type ReadHalf: AsyncRead + Send + Sync + Unpin + 'static;
    type WriteHalf: AsyncWrite + Send + Sync + Unpin + 'static;

    fn make_transport(
        &self,
        addr: Address,
    ) -> impl Future<Output = io::Result<(Self::ReadHalf, Self::WriteHalf)>> + Send;
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
        #[cfg_attr(docsrs, doc(cfg(feature = "rustls")))]
        #[cfg(feature = "rustls")]
        Rustls(tokio_rustls::TlsConnector),

        /// This takes an `Arc` because `tokio_native_tls::TlsConnector` does not internally use `Arc`
        #[cfg_attr(docsrs, doc(cfg(feature = "native-tls")))]
        #[cfg(feature = "native-tls")]
        NativeTls(std::sync::Arc<tokio_native_tls::TlsConnector>),
    }

    impl std::fmt::Debug for TlsConnector {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                #[cfg(feature = "rustls")]
                Self::Rustls(_) => f.debug_tuple("Rustls").finish(),

                #[cfg(feature = "native-tls")]
                Self::NativeTls(_) => f.debug_tuple("NativeTls").finish(),
            }
        }
    }

    /// TLS config for client
    #[derive(Debug, Clone)]
    pub struct ClientTlsConfig {
        pub server_name: String,
        pub connector: TlsConnector,
    }

    impl ClientTlsConfig {
        pub fn new(server_name: impl Into<String>, connector: impl Into<TlsConnector>) -> Self {
            Self {
                server_name: server_name.into(),
                connector: connector.into(),
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct TlsMakeTransport {
        cfg: Config,
        tls_config: ClientTlsConfig,
    }

    impl TlsMakeTransport {
        pub fn new(cfg: Config, tls_config: ClientTlsConfig) -> Self {
            Self {
                cfg,
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
                            let server_name = librustls::ServerName::try_from(&self.tls_config.server_name[..])
                                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                            connector
                                .connect(server_name, tcp)
                                .await
                                .map(tokio_rustls::TlsStream::Client)
                                .map(Conn::from)
                        }
                        #[cfg(feature = "native-tls")]
                        TlsConnector::NativeTls(connector) => {
                            let tcp = make_tcp_connection(&self.cfg, addr).await?;
                            connector
                                .connect(&self.tls_config.server_name[..], tcp)
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

    impl MakeTransport for TlsMakeTransport {
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

cfg_rustls! {
    impl From<tokio_rustls::TlsConnector> for TlsConnector {
        fn from(value: tokio_rustls::TlsConnector) -> Self {
            Self::Rustls(value)
        }
    }
}

cfg_native_tls! {
    impl From<std::sync::Arc<tokio_native_tls::TlsConnector>> for TlsConnector {
        fn from(value: std::sync::Arc<tokio_native_tls::TlsConnector>) -> Self {
            Self::NativeTls(value)
        }
    }

    impl From<tokio_native_tls::TlsConnector> for TlsConnector {
        fn from(value: tokio_native_tls::TlsConnector) -> Self {
            Self::NativeTls(std::sync::Arc::new(value))
        }
    }
}
