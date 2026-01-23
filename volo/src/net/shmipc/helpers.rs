use std::io;

use ::shmipc::stream::Stream;

// Given the strong coupling between `Stream` and IO interfaces, and the inability to sufficiently
// decouple them, it was necessary to introduce this Helper to manager lifetimes of `Stream`.
#[derive(Default)]
pub struct ShmipcHelper {
    inner: Option<Box<Stream>>,
}

impl ShmipcHelper {
    pub const fn none() -> Self {
        Self { inner: None }
    }

    pub const fn new(inner: Box<Stream>) -> Self {
        Self { inner: Some(inner) }
    }

    pub const fn available(&self) -> bool {
        self.inner.is_some()
    }

    /// Close the current stream.
    ///
    /// NOTE: Server-side [`Stream`] MUST be closed after using it!
    pub async fn close(&mut self) -> io::Result<()> {
        if let Some(s) = &mut self.inner {
            s.close().await.map_err(Into::into)
        } else {
            Ok(())
        }
    }

    /// Put the current stream into pool of its [`SessionManager`].
    ///
    /// NOTE: Client-side [`Stream`] MUST be reused after using it!
    ///
    /// [`SessionManager`]: ::shmipc::session::SessionManager
    pub async fn reuse(&self) {
        if let Some(s) = &self.inner {
            s.reuse().await
        }
    }

    /// Release current read buffer and put it into its pool.
    ///
    /// NOTE: This function MUST be called after reading data!
    pub fn release_read_and_reuse(&self) {
        if let Some(s) = &self.inner {
            s.release_read_and_reuse();
        }
    }

    pub fn close_guard(&self) -> ShmipcCloseGuard {
        ShmipcCloseGuard {
            inner: self.inner.clone(),
        }
    }
}

pub struct ShmipcCloseGuard {
    inner: Option<Box<Stream>>,
}

impl Drop for ShmipcCloseGuard {
    fn drop(&mut self) {
        if let Some(mut stream) = self.inner.take() {
            tokio::spawn(async move {
                let _ = stream.close().await;
            });
        }
    }
}
