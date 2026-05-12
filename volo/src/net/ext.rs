use futures::Future;
use tokio::io::{self, Interest, Ready};

use super::conn::{Conn, ConnStream, OwnedReadHalf, OwnedWriteHalf};

/// Asynchronous extension functions.
pub trait AsyncExt {
    /// Checks for IO readiness.
    ///
    /// See [`tokio::net::TcpStream::ready`] for details.
    fn ready(&self, interest: Interest) -> impl Future<Output = io::Result<Ready>> + Send;
}

impl AsyncExt for Conn {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match &self.stream {
            ConnStream::Tcp(stream) => stream.ready(interest).await,
            #[cfg(target_family = "unix")]
            ConnStream::Unix(stream) => stream.ready(interest).await,
            #[cfg(feature = "rustls")]
            ConnStream::Rustls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "native-tls")]
            ConnStream::NativeTls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "named-pipe")]
            ConnStream::NamedPipeClient(_) | ConnStream::NamedPipeServer(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for NamedPipe connection",
            )),
        }
    }
}

impl AsyncExt for OwnedReadHalf {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match self {
            OwnedReadHalf::Tcp(half) => half.ready(interest).await,
            #[cfg(target_family = "unix")]
            OwnedReadHalf::Unix(half) => half.ready(interest).await,
            #[cfg(feature = "rustls")]
            OwnedReadHalf::Rustls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "native-tls")]
            OwnedReadHalf::NativeTls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "named-pipe")]
            OwnedReadHalf::NamedPipeClient(_) | OwnedReadHalf::NamedPipeServer(_) => {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "AsyncExt is not supported for NamedPipe connection",
                ))
            }
        }
    }
}

impl AsyncExt for OwnedWriteHalf {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match self {
            OwnedWriteHalf::Tcp(half) => half.ready(interest).await,
            #[cfg(target_family = "unix")]
            OwnedWriteHalf::Unix(half) => half.ready(interest).await,
            #[cfg(feature = "rustls")]
            OwnedWriteHalf::Rustls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalf::NativeTls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "named-pipe")]
            OwnedWriteHalf::NamedPipeClient(_) | OwnedWriteHalf::NamedPipeServer(_) => {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "AsyncExt is not supported for NamedPipe connection",
                ))
            }
        }
    }
}
