use futures::Future;
use tokio::io::{self, Interest, Ready};

use super::conn::{OwnedReadHalf, OwnedWriteHalf};
use crate::net::conn::{Conn, ConnStream};

/// Asynchronous extension functions.
pub trait AsyncExt {
    /// Checks for IO readiness.
    ///
    /// See [`tokio::net::TcpStream::ready`] for details.
    fn ready(&self, interest: Interest) -> impl Future<Output = io::Result<Ready>> + Send;

    /// Get helper of ShmIPC.
    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> super::shmipc::ShmipcHelper {
        super::shmipc::ShmipcHelper::none()
    }
}

impl AsyncExt for Conn {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match &self.stream {
            ConnStream::Tcp(stream) => stream.ready(interest).await,
            #[cfg(target_family = "unix")]
            ConnStream::Unix(stream) => stream.ready(interest).await,
            #[cfg(feature = "__tls")]
            ConnStream::Tls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "shmipc")]
            ConnStream::Shmipc(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for ShmIPC connection",
            )),
        }
    }

    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> super::shmipc::ShmipcHelper {
        match &self.stream {
            ConnStream::Shmipc(stream) => stream.helper(),
            _ => Default::default(),
        }
    }
}

impl AsyncExt for OwnedReadHalf {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match self {
            OwnedReadHalf::Tcp(half) => half.ready(interest).await,
            #[cfg(target_family = "unix")]
            OwnedReadHalf::Unix(half) => half.ready(interest).await,
            #[cfg(feature = "__tls")]
            OwnedReadHalf::Tls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "shmipc")]
            OwnedReadHalf::Shmipc(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for ShmIPC connection",
            )),
        }
    }

    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> super::shmipc::ShmipcHelper {
        match self {
            OwnedReadHalf::Shmipc(rh) => rh.helper(),
            _ => Default::default(),
        }
    }
}

impl AsyncExt for OwnedWriteHalf {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        match self {
            OwnedWriteHalf::Tcp(half) => half.ready(interest).await,
            #[cfg(target_family = "unix")]
            OwnedWriteHalf::Unix(half) => half.ready(interest).await,
            #[cfg(feature = "__tls")]
            OwnedWriteHalf::Tls(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for TLS connection",
            )),
            #[cfg(feature = "shmipc")]
            OwnedWriteHalf::Shmipc(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "AsyncExt is not supported for ShmIPC connection",
            )),
        }
    }

    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> super::shmipc::ShmipcHelper {
        match self {
            OwnedWriteHalf::Shmipc(wh) => wh.helper(),
            _ => Default::default(),
        }
    }
}
