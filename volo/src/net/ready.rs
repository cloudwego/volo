use futures::Future;
use tokio::io::{self, Interest, Ready};

use super::conn::{OwnedReadHalf, OwnedWriteHalf};

/// Asynchronous IO readiness.
///
/// Like [`tokio::io::AsyncRead`] or [`tokio::io::AsyncWrite`], but for
/// readiness events.
pub trait AsyncReady {
    /// Checks for IO readiness.
    ///
    /// See [`tokio::net::TcpStream::ready`] for details.
    fn ready(&self, interest: Interest) -> impl Future<Output = io::Result<Ready>> + Send;
}

impl AsyncReady for OwnedReadHalf {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match self {
            OwnedReadHalf::Tcp(half) => half.ready(interest).await,
            #[cfg(target_family = "unix")]
            OwnedReadHalf::Unix(half) => half.ready(interest).await,
            #[cfg(feature = "rustls")]
            OwnedReadHalf::Rustls(_) => todo!(),
            #[cfg(feature = "native-tls")]
            OwnedReadHalf::NativeTls(_) => todo!(),
            #[cfg(feature = "named-pipe")]
            OwnedReadHalf::NamedPipeClient(_) | OwnedReadHalf::NamedPipeServer(_) => {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "AsyncReady is not supported for NamedPipe connection",
                ))
            }
        }
    }
}

impl AsyncReady for OwnedWriteHalf {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match self {
            OwnedWriteHalf::Tcp(half) => half.ready(interest).await,
            #[cfg(target_family = "unix")]
            OwnedWriteHalf::Unix(half) => half.ready(interest).await,
            #[cfg(feature = "rustls")]
            OwnedWriteHalf::Rustls(_) => todo!(),
            #[cfg(feature = "native-tls")]
            OwnedWriteHalf::NativeTls(_) => todo!(),
            #[cfg(feature = "named-pipe")]
            OwnedWriteHalf::NamedPipeClient(_) | OwnedWriteHalf::NamedPipeServer(_) => {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "AsyncReady is not supported for NamedPipe connection",
                ))
            }
        }
    }
}
