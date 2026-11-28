use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, Interest, ReadBuf, Ready};

use super::{Conn, ConnInfo, OwnedReadHalf, OwnedWriteHalf};
use crate::{net::ext::AsyncExt, util::buf_reader::BufReader};

/// A connection whose first eight application bytes can be inspected and replayed.
///
/// The original duplex connection, including its TLS session, remains intact until
/// [`Self::into_buffered_split`] explicitly transfers it to a split transport.
pub struct PeekedConn {
    conn: Conn,
    prefix: [u8; 8],
    filled: usize,
    replayed: usize,
}

/// The original read/write halves with the unread probe bytes installed in the reader.
pub struct BufferedConnParts {
    pub info: ConnInfo,
    pub is_tls: bool,
    pub negotiated_alpn: Option<Vec<u8>>,
    pub reader: BufReader<OwnedReadHalf>,
    pub writer: OwnedWriteHalf,
}

impl BufferedConnParts {
    /// Transfers an already selected single-protocol connection without probing it.
    pub fn from_conn(conn: Conn, read_capacity: usize) -> Self {
        #[cfg(feature = "__tls")]
        let is_tls = conn.stream.is_tls();
        #[cfg(not(feature = "__tls"))]
        let is_tls = false;
        let negotiated_alpn = conn.stream.negotiated_alpn();
        let (reader, writer) = conn.stream.into_split();
        Self {
            info: conn.info,
            is_tls,
            negotiated_alpn,
            reader: BufReader::with_capacity(read_capacity, reader),
            writer,
        }
    }
}

impl PeekedConn {
    /// Reads exactly eight bytes without consuming bytes beyond the prefix.
    ///
    /// An empty connection returns `None`. EOF after a partial prefix is an error.
    /// Dropping this future also drops the connection.
    pub async fn peek8(conn: Conn) -> io::Result<Option<Self>> {
        Self::peek_until(conn, |prefix| prefix.len() == 8).await
    }

    /// Inspect at most eight bytes, stopping once `decided` can select a protocol.
    /// Only bytes actually read are replayed to the selected transport.
    pub async fn peek_until(
        mut conn: Conn,
        decided: impl Fn(&[u8]) -> bool,
    ) -> io::Result<Option<Self>> {
        let mut prefix = [0; 8];
        let mut filled = 0;
        while filled < prefix.len() {
            let count = conn.read(&mut prefix[filled..]).await?;
            if count == 0 {
                if filled == 0 {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed during protocol detection",
                ));
            }
            filled += count;
            if decided(&prefix[..filled]) {
                break;
            }
        }
        Ok(Some(Self {
            conn,
            prefix,
            filled,
            replayed: 0,
        }))
    }

    pub fn prefix(&self) -> &[u8; 8] {
        &self.prefix
    }

    pub fn info(&self) -> &ConnInfo {
        &self.conn.info
    }

    pub fn transport(&self) -> super::ConnTransport {
        self.conn.stream.transport()
    }

    pub fn is_tls(&self) -> bool {
        #[cfg(feature = "__tls")]
        {
            self.conn.stream.is_tls()
        }
        #[cfg(not(feature = "__tls"))]
        {
            false
        }
    }

    pub fn negotiated_alpn(&self) -> Option<Vec<u8>> {
        self.conn.stream.negotiated_alpn()
    }

    /// Splits once, preserving the original connection info and all unread bytes.
    pub fn into_buffered_split(self, read_capacity: usize) -> BufferedConnParts {
        let is_tls = self.is_tls();
        let negotiated_alpn = self.negotiated_alpn();
        let (reader, writer) = self.conn.stream.into_split();
        BufferedConnParts {
            info: self.conn.info,
            is_tls,
            negotiated_alpn,
            reader: BufReader::with_capacity_and_prefix(
                read_capacity,
                reader,
                &self.prefix[self.replayed..self.filled],
            ),
            writer,
        }
    }
}

impl AsyncRead for PeekedConn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.replayed < self.filled {
            let count = buf.remaining().min(self.filled - self.replayed);
            buf.put_slice(&self.prefix[self.replayed..self.replayed + count]);
            self.replayed += count;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.conn).poll_read(cx, buf)
    }
}

impl AsyncWrite for PeekedConn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.conn).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.conn).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.conn).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.conn).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.conn.is_write_vectored()
    }
}

impl AsyncExt for PeekedConn {
    async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        if interest.is_readable() && self.replayed < self.filled {
            return Ok(Ready::READABLE);
        }
        self.conn.ready(interest).await
    }

    #[cfg(feature = "shmipc")]
    fn is_shmipc(&self) -> bool {
        self.conn.is_shmipc()
    }

    #[cfg(feature = "shmipc")]
    fn shmipc_helper(&self) -> crate::net::shmipc::ShmipcHelper {
        self.conn.shmipc_helper()
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;
    use tokio::{
        io::AsyncWriteExt,
        net::{TcpListener, TcpStream},
    };

    use super::*;
    use crate::net::Address;

    async fn tcp_pair() -> (TcpStream, Conn) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server.into())
    }

    #[tokio::test]
    async fn a_complete_six_byte_compact_call_can_be_dispatched_without_an_eighth_byte() {
        // Compact CALL, seq=0, one-byte method "m", empty argument struct.
        let request = [0x82, 0x21, 0, 1, b'm', 0];
        let (mut client, conn) = tcp_pair().await;
        client.write_all(&request).await.unwrap();
        let mut conn = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            PeekedConn::peek_until(conn, |bytes| bytes.len() >= 6 && bytes[4..6] != [0x10, 0]),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let mut actual = [0; 6];
        conn.read_exact(&mut actual).await.unwrap();
        assert_eq!(actual, request);
        conn.write_all(b"reply").await.unwrap();
        client.read_exact(&mut [0; 5]).await.unwrap();
        // The unused part of the eight-byte storage must not be replayed.
        assert!(conn.read(&mut [0; 1]).now_or_never().is_none());
    }

    #[tokio::test]
    async fn fragmented_prefix_replays_with_small_reads_and_preserves_tail() {
        const INPUT: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\npayload";
        for first_chunk in 1..=8 {
            let (mut client, mut conn) = tcp_pair().await;
            let peer = Address::Ip("192.0.2.1:1234".parse().unwrap());
            conn.info.peer_addr = Some(peer.clone());
            client.write_all(&INPUT[..first_chunk]).await.unwrap();
            let peek = PeekedConn::peek8(conn);
            tokio::pin!(peek);
            if first_chunk < 8 {
                assert!(futures::poll!(&mut peek).is_pending());
            }
            client.write_all(&INPUT[first_chunk..]).await.unwrap();
            client.shutdown().await.unwrap();
            let mut conn = peek.await.unwrap().unwrap();
            assert_eq!(conn.prefix(), b"PRI * HT");
            assert_eq!(conn.prefix(), b"PRI * HT");
            assert_eq!(conn.info().peer_addr, Some(peer));
            assert!(!conn.is_tls());
            assert_eq!(conn.negotiated_alpn(), None);
            assert!(conn.ready(Interest::READABLE).now_or_never().is_some());
            assert_eq!(conn.read(&mut []).await.unwrap(), 0);

            let mut actual = Vec::new();
            let mut small = [0; 3];
            loop {
                let count = conn.read(&mut small).await.unwrap();
                if count == 0 {
                    break;
                }
                actual.extend_from_slice(&small[..count]);
            }
            assert_eq!(actual, INPUT);
        }
    }

    #[tokio::test]
    async fn split_replays_only_unread_prefix_and_keeps_original_halves() {
        let (mut client, mut original) = tcp_pair().await;
        original.info.peer_addr = None;
        client.write_all(b"12345678tail").await.unwrap();
        client.shutdown().await.unwrap();
        let mut conn = PeekedConn::peek8(original).await.unwrap().unwrap();
        let mut consumed = [0; 3];
        conn.read_exact(&mut consumed).await.unwrap();
        assert_eq!(&consumed, b"123");
        let mut parts = conn.into_buffered_split(1);
        assert_eq!(parts.reader.buffer(), b"45678");
        assert_eq!(parts.info.peer_addr, None);
        assert!(!parts.is_tls);
        assert_eq!(parts.negotiated_alpn, None);
        #[cfg(feature = "shmipc")]
        {
            // These calls compile against the public trait on all three native types.
            let _ = AsyncExt::shmipc_helper(parts.reader.get_ref());
            let _ = AsyncExt::shmipc_helper(&parts.reader);
            let _ = AsyncExt::shmipc_helper(&parts.writer);
        }
        let mut remaining = Vec::new();
        parts.reader.read_to_end(&mut remaining).await.unwrap();
        assert_eq!(remaining, b"45678tail");
        parts.writer.write_all(b"reply").await.unwrap();
        parts.writer.shutdown().await.unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, b"reply");
    }

    #[tokio::test]
    async fn eof_and_cancel_during_detection_close_the_connection() {
        for length in 0..8 {
            let (mut client, conn) = tcp_pair().await;
            client.write_all(&b"12345678"[..length]).await.unwrap();
            client.shutdown().await.unwrap();
            let result = PeekedConn::peek8(conn).await;
            if length == 0 {
                assert!(result.unwrap().is_none());
            } else {
                assert_eq!(result.err().unwrap().kind(), io::ErrorKind::UnexpectedEof);
            }
        }
        let (mut client, conn) = tcp_pair().await;
        assert!(PeekedConn::peek8(conn).now_or_never().is_none());
        assert_eq!(client.read(&mut [0; 1]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn vectored_writes_flush_and_shutdown_reach_the_original_connection() {
        let (mut client, conn) = tcp_pair().await;
        client.write_all(b"12345678").await.unwrap();
        let mut conn = PeekedConn::peek8(conn).await.unwrap().unwrap();
        assert!(conn.is_write_vectored());
        let count = conn
            .write_vectored(&[io::IoSlice::new(b"hello"), io::IoSlice::new(b"world")])
            .await
            .unwrap();
        conn.write_all(&b"helloworld"[count..]).await.unwrap();
        conn.flush().await.unwrap();
        conn.shutdown().await.unwrap();
        let mut actual = Vec::new();
        client.read_to_end(&mut actual).await.unwrap();
        assert_eq!(actual, b"helloworld");
    }

    #[cfg(target_family = "unix")]
    #[tokio::test]
    async fn unix_connection_replays_without_changing_peer_info() {
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        client.write_all(b"12345678unix").await.unwrap();
        client.shutdown().await.unwrap();
        let original: Conn = server.into();
        let peer = original.info.peer_addr.clone();
        let conn = PeekedConn::peek8(original).await.unwrap().unwrap();
        let mut parts = conn.into_buffered_split(512);
        assert_eq!(parts.info.peer_addr, peer);
        assert!(matches!(parts.reader.get_ref(), OwnedReadHalf::Unix(_)));
        assert!(matches!(&parts.writer, OwnedWriteHalf::Unix(_)));
        let mut actual = Vec::new();
        parts.reader.read_to_end(&mut actual).await.unwrap();
        assert_eq!(actual, b"12345678unix");
    }
}
