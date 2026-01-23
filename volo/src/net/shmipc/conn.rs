use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;
use shmipc::{compact::StreamExt, stream};
use tokio::io::{AsyncRead, AsyncWrite};

use super::addr::Address;

pub struct Stream {
    inner: StreamExt,
    addr: super::addr::Address,
}

#[pin_project]
pub struct ReadHalf {
    #[pin]
    inner: Box<Stream>,
}

#[pin_project]
pub struct WriteHalf {
    #[pin]
    inner: Box<Stream>,
}

impl Stream {
    pub fn new(inner: stream::Stream) -> Self {
        let session_id = inner.session_id();
        let stream_id = inner.stream_id();
        Self {
            inner: StreamExt::new(inner),
            addr: super::addr::Address::Client(session_id, stream_id),
        }
    }

    pub fn helper(&self) -> super::ShmipcHelper {
        super::ShmipcHelper::new(Box::new(self.inner.inner().clone()))
    }

    fn dup(self) -> (Self, Self) {
        let raw_stream = self.inner.into_inner();
        let rh = Self {
            inner: StreamExt::new(raw_stream.clone()),
            addr: self.addr.clone(),
        };
        let wh = Self {
            inner: StreamExt::new(raw_stream),
            addr: self.addr,
        };
        (rh, wh)
    }

    pub fn into_split(self) -> (ReadHalf, WriteHalf) {
        let (rh, wh) = self.dup();
        (ReadHalf::new(rh), WriteHalf::new(wh))
    }

    pub fn peer_addr(&self) -> super::addr::Address {
        self.addr.clone()
    }
}

impl ReadHalf {
    pub fn new(inner: Stream) -> Self {
        Self {
            inner: Box::new(inner),
        }
    }

    pub fn helper(&self) -> super::ShmipcHelper {
        super::ShmipcHelper::new(Box::new(self.inner.inner.inner().clone()))
    }
}

impl WriteHalf {
    pub fn new(inner: Stream) -> Self {
        Self {
            inner: Box::new(inner),
        }
    }

    pub fn helper(&self) -> super::ShmipcHelper {
        super::ShmipcHelper::new(Box::new(self.inner.inner.inner().clone()))
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.get_mut().inner), cx, buf)
    }
}

impl AsyncRead for ReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(self.project().inner, cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        AsyncWrite::poll_write(Pin::new(&mut self.get_mut().inner), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.get_mut().inner), cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.get_mut().inner), cx)
    }
}

impl AsyncWrite for WriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        AsyncWrite::poll_write(self.project().inner, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_flush(self.project().inner, cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_shutdown(self.project().inner, cx)
    }
}

#[derive(Debug)]
pub struct Listener {
    inner: shmipc::Listener,
}

impl From<shmipc::Listener> for Listener {
    fn from(value: shmipc::Listener) -> Self {
        Self { inner: value }
    }
}

impl Listener {
    pub async fn listen(
        addr: Address,
        config: Option<shmipc::config::Config>,
    ) -> Result<Self, io::Error> {
        let config = config.unwrap_or_else(super::config::shmipc_config);

        match addr {
            Address::Tcp(tcp) => {
                if !tcp.ip().is_loopback() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        "shmipc can only use loopback address",
                    ));
                }
                shmipc::Listener::new(shmipc::transport::DefaultTcpListen, tcp, config)
                    .await
                    .map(Into::into)
            }
            Address::Unix(uds) => {
                shmipc::Listener::new(shmipc::transport::DefaultUnixListen, uds, config)
                    .await
                    .map(Into::into)
            }
            Address::Client(_, _) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "client address cannot be listened",
            )),
        }
    }

    pub async fn accept(&mut self) -> io::Result<Stream> {
        self.inner.accept().await.map(Stream::new)
    }
}

#[derive(Debug)]
pub struct ListenerStream {
    inner: shmipc::Listener,
}

impl ListenerStream {
    pub fn new(listener: Listener) -> Self {
        Self {
            inner: listener.inner,
        }
    }

    pub fn into_inner(self) -> Listener {
        Listener { inner: self.inner }
    }
}

impl futures::stream::Stream for ListenerStream {
    type Item = io::Result<Stream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.poll_accept(cx) {
            Poll::Ready(res) => Poll::Ready(Some(res.map(Stream::new))),
            Poll::Pending => Poll::Pending,
        }
    }
}
