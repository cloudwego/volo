//! These codes are copied from `tokio/io/util/buf_reader.rs`.
//! There's some modify of this code, such as support compact.

use std::{
    cmp, fmt, io,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, ReadBuf};

// used by `BufReader` and `BufWriter`
// https://github.com/rust-lang/rust/blob/master/library/std/src/sys_common/io.rs#L1
const DEFAULT_BUF_SIZE: usize = 8 * 1024;

macro_rules! ready {
    ($e:expr $(,)?) => {
        match $e {
            std::task::Poll::Ready(t) => t,
            std::task::Poll::Pending => return std::task::Poll::Pending,
        }
    };
}

impl<R: AsyncRead + Unpin> BufReader<R> {
    pub async fn fill_buf_at_least(&mut self, len: usize) -> io::Result<&[u8]> {
        if self.len >= len {
            return Ok(&self.buf[self.pos..self.len]);
        }

        assert!(len < self.cap);
        if len > (self.cap - self.pos) {
            self.compact();
        }

        if self.pos >= self.cap {
            debug_assert!(self.pos == self.cap);
            let size = self.inner.read(&mut self.buf).await?;
            self.len = size;
            self.pos = 0
        } else if self.len < self.cap {
            while self.len < len {
                let buf = &mut self.buf[self.len..self.cap];
                let size = self.inner.read(buf).await?;
                if size == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid eof",
                    ));
                }
                self.len += size;
            }
        }
        Ok(&self.buf[self.pos..self.len])
    }
}

/// The `BufReader` struct adds buffering to any reader.
///
/// It can be excessively inefficient to work directly with a [`AsyncRead`]
/// instance. A `BufReader` performs large, infrequent reads on the underlying
/// [`AsyncRead`] and maintains an in-memory buffer of the results.
///
/// `BufReader` can improve the speed of programs that make *small* and
/// *repeated* read calls to the same file or network socket. It does not
/// help when reading very large amounts at once, or reading just one or a few
/// times. It also provides no advantage when reading from a source that is
/// already in memory, like a `Vec<u8>`.
///
/// When the `BufReader` is dropped, the contents of its buffer will be
/// discarded. Creating multiple instances of a `BufReader` on the same
/// stream can cause data loss.
#[pin_project]
pub struct BufReader<R> {
    #[pin]
    pub(super) inner: R,
    pub(super) buf: Box<[u8]>,
    pub(super) pos: usize,
    pub(super) len: usize, // the current valid index
    pub(super) cap: usize,
}

impl<R: AsyncRead> BufReader<R> {
    /// Creates a new `BufReader` with a default buffer capacity. The default is currently 8 KB,
    /// but may change in the future.
    pub fn new(inner: R) -> Self {
        Self::with_capacity(DEFAULT_BUF_SIZE, inner)
    }

    /// Creates a new `BufReader` with the specified buffer capacity.
    pub fn with_capacity(capacity: usize, inner: R) -> Self {
        let buffer = vec![0; capacity];
        Self {
            inner,
            buf: buffer.into_boxed_slice(),
            pos: 0,
            len: 0,
            cap: capacity,
        }
    }

    /// Gets a reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Gets a mutable reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Gets a pinned mutable reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_pin_mut(self: Pin<&mut Self>) -> Pin<&mut R> {
        self.project().inner
    }

    /// Consumes this `BufReader`, returning the underlying reader.
    ///
    /// Note that any leftover data in the internal buffer is lost.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Returns a reference to the internally buffered data.
    ///
    /// Unlike `fill_buf`, this will not attempt to fill the buffer if it is empty.
    pub fn buffer(&self) -> &[u8] {
        &self.buf[self.pos..self.len]
    }

    /// 整理 buffer，移动到最前。
    pub fn compact(&mut self) {
        if self.len == self.pos {
            self.pos = 0;
            self.len = 0;
            return;
        }

        let len = self.len - self.pos;
        let dst = self.buf.as_mut_ptr();
        let src = unsafe { dst.add(self.pos) };

        unsafe {
            std::ptr::copy(src, dst, len);
        }

        self.pos = 0;
        self.len = len;
    }

    pub fn clear(&mut self) {
        self.pos = 0;
        self.len = 0;
    }

    /// Invalidates all data in the internal buffer.
    #[inline]
    fn discard_buffer(self: Pin<&mut Self>) {
        let me = self.project();
        *me.pos = 0;
        *me.len = 0;
    }
}

impl<R: AsyncRead> AsyncRead for BufReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // If we don't have any buffered data and we're doing a massive read
        // (larger than our internal buffer), bypass our internal buffer
        // entirely.
        if self.pos == self.len && buf.remaining() >= self.buf.len() {
            let res = ready!(self.as_mut().get_pin_mut().poll_read(cx, buf));
            self.discard_buffer();
            return Poll::Ready(res);
        }
        let rem = ready!(self.as_mut().poll_fill_buf(cx))?;
        let amt = std::cmp::min(rem.len(), buf.remaining());
        buf.put_slice(&rem[..amt]);
        self.consume(amt);
        Poll::Ready(Ok(()))
    }
}

impl<R: AsyncRead> AsyncBufRead for BufReader<R> {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
        let me = self.project();

        // If we've reached the end of our internal buffer then we need to fetch
        // some more data from the underlying reader.
        // Branch using `>=` instead of the more correct `==`
        // to tell the compiler that the pos..cap slice is always valid.
        if *me.pos >= *me.cap {
            debug_assert!(*me.pos == *me.cap);
            let mut buf = ReadBuf::new(me.buf);
            ready!(me.inner.poll_read(cx, &mut buf))?;
            *me.len = buf.filled().len();
            *me.pos = 0;
        } else if *me.len < *me.cap {
            // We have some buffer
            let mut buf = ReadBuf::new(&mut me.buf[*me.len..*me.cap]);
            match me.inner.poll_read(cx, &mut buf) {
                Poll::Ready(t) => t,
                Poll::Pending => {
                    if *me.pos < *me.len {
                        return Poll::Ready(Ok(&me.buf[*me.pos..*me.len]));
                    }
                    return Poll::Pending;
                }
            }?;
            *me.len += buf.filled().len();
        }
        Poll::Ready(Ok(&me.buf[*me.pos..*me.len]))
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        let me = self.project();
        *me.pos = cmp::min(*me.pos + amt, *me.len);
    }
}

impl<R: fmt::Debug> fmt::Debug for BufReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufReader")
            .field("reader", &self.inner)
            .field(
                "buffer",
                &format_args!("{}/{}", self.len - self.pos, self.buf.len()),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(test)]
    fn is_unpin<T: Unpin>() {}

    #[test]
    fn assert_unpin() {
        is_unpin::<BufReader<()>>();
    }

    #[test]
    fn test_compact() {
        let mut v = vec![0; 16];
        v[0] = 1;
        v[1] = 2;
        v[2] = 3;
        v[3] = 4;
        v[4] = 5;
        let mut buf = BufReader {
            inner: tokio::io::empty(),
            buf: v.into_boxed_slice(),
            pos: 0,
            len: 5,
            cap: 16,
        };
        buf.compact();
        assert_eq!(buf.pos, 0);
        assert_eq!(buf.len, 5);
        assert_eq!(buf.buf[buf.pos..buf.len], [1, 2, 3, 4, 5]);
        let mut v = vec![0; 16];
        v[0] = 1;
        v[1] = 2;
        v[2] = 3;
        v[3] = 4;
        v[4] = 5;
        v[5] = 6;
        v[6] = 7;
        v[7] = 8;
        v[8] = 9;
        v[9] = 10;
        let mut buf = BufReader {
            inner: tokio::io::empty(),
            buf: v.into_boxed_slice(),
            pos: 5,
            len: 10,
            cap: 16,
        };
        buf.compact();
        assert_eq!(buf.pos, 0);
        assert_eq!(buf.len, 5);
        assert_eq!(buf.buf[buf.pos..buf.len], [6, 7, 8, 9, 10]);
    }
}
