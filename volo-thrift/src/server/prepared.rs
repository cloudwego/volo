use std::{
    io,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use futures::future::BoxFuture;
use motore::service::Service;
use scopeguard::defer;
use tokio::sync::Notify;
use volo::net::conn::{BufferedConnParts, Conn, OwnedReadHalf, OwnedWriteHalf};

use super::{StreamingServerState, TraceFn};
use crate::{
    EntryMessage, ServerError, codec::MakeCodec, context::ServerContext, tracing::SpanProvider,
};

/// Server-owned connection signals. A peer disconnect never sets these signals.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct ConnectionControl {
    pub(super) exit_notify: Arc<Notify>,
    pub(super) exit_mark: Arc<AtomicBool>,
    reads_closed: Arc<AtomicBool>,
    pub(super) streaming_exit: Arc<Notify>,
    pub(super) connections: Arc<AtomicUsize>,
}

impl ConnectionControl {
    /// Stops TTStream admission and marks subsequent unary responses for crrst.
    pub fn begin_drain(&self) {
        self.exit_mark.store(true, Ordering::Release);
        self.streaming_exit.notify_waiters();
    }

    /// Stops idle unary reads after the server's crrst grace period.
    /// Accepted ping-pong work still runs to completion.
    pub fn close_idle(&self) {
        self.reads_closed.store(true, Ordering::Release);
        self.exit_notify.notify_waiters();
    }

    async fn idle_closed(&self) {
        let notified = self.exit_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.reads_closed.load(Ordering::Acquire) {
            notified.await;
        }
    }

    pub fn active_connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }
}

/// The selected Thrift driver. Detection belongs to the listener's owner.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum DetectedThrift {
    PingPong,
    /// The unary codec selects serial Buffered or configured framed multiplexing.
    Unary,
    Ttstream,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UnaryMode {
    Explicit,
    Serial,
    Detected,
}

/// A prepared service and its server-level hooks; this type never binds a listener.
#[doc(hidden)]
pub struct PreparedServer<Svc, Req, MkC, SP> {
    pub(super) driver: ConnectionDriver<Svc, Req, MkC, SP>,
    pub(super) shutdown_hooks: Vec<Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>>,
}

impl<Svc, Req, MkC: Clone, SP: Clone> PreparedServer<Svc, Req, MkC, SP> {
    pub fn connection(&self) -> ConnectionDriver<Svc, Req, MkC, SP> {
        self.driver.clone()
    }

    pub fn control(&self) -> ConnectionControl {
        self.driver.control.clone()
    }

    pub async fn run_shutdown_hooks(&mut self) {
        for hook in std::mem::take(&mut self.shutdown_hooks) {
            hook().await;
        }
    }
}

/// A single connection's native driver, without an accept loop or signal handler.
#[doc(hidden)]
pub struct ConnectionDriver<Svc, Req, MkC, SP> {
    pub(super) service: Arc<Svc>,
    pub(super) streaming: Option<Arc<StreamingServerState>>,
    pub(super) make_codec: MkC,
    pub(super) unsafe_codec: Option<bool>,
    pub(super) stat_tracer: Arc<[TraceFn]>,
    pub(super) control: ConnectionControl,
    #[cfg(feature = "multiplex")]
    pub(super) multiplex: bool,
    pub(super) span_provider: SP,
    pub(super) _marker: PhantomData<fn(Req)>,
}

impl<Svc, Req, MkC: Clone, SP: Clone> Clone for ConnectionDriver<Svc, Req, MkC, SP> {
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            streaming: self.streaming.clone(),
            make_codec: self.make_codec.clone(),
            unsafe_codec: self.unsafe_codec,
            stat_tracer: self.stat_tracer.clone(),
            control: self.control.clone(),
            #[cfg(feature = "multiplex")]
            multiplex: self.multiplex,
            span_provider: self.span_provider.clone(),
            _marker: PhantomData,
        }
    }
}

impl<Svc, Req, Resp, MkC, SP> ConnectionDriver<Svc, Req, MkC, SP>
where
    Svc: Service<ServerContext, Req, Response = Resp, Error = ServerError> + Send + Sync + 'static,
    Req: EntryMessage + 'static,
    Resp: EntryMessage + 'static,
    MkC: MakeCodec<OwnedReadHalf, OwnedWriteHalf>,
    SP: SpanProvider,
{
    /// The original explicit Thrift path, including its optional TTStream probe.
    pub async fn serve(self, conn: Conn) {
        let connections = self.control.connections.clone();
        connections.fetch_add(1, Ordering::AcqRel);
        defer! { connections.fetch_sub(1, Ordering::AcqRel); }
        let peer_addr = conn.info.peer_addr;
        let (reader, writer) = conn.stream.into_split();
        #[cfg(all(feature = "multiplex", feature = "shmipc"))]
        if self.multiplex
            && peer_addr
                .as_ref()
                .is_some_and(volo::net::Address::is_shmipc)
            && self.streaming.is_none()
        {
            tracing::error!("multiplex is not supported when using shmipc");
            let _ = reader.shmipc_helper().close().await;
            return;
        }
        let (encoder, decoder) = self.make_codec.make_codec(reader, writer);
        let (encoder, decoder) = if let Some(streaming) = self.streaming.clone() {
            match super::dispatch_streaming(
                encoder,
                decoder,
                streaming,
                self.control.exit_mark.clone(),
                peer_addr.clone(),
            )
            .await
            {
                Some(parts) => parts,
                None => return,
            }
        } else {
            (encoder, decoder)
        };
        self.serve_unary(encoder, decoder, peer_addr, UnaryMode::Explicit)
            .await;
    }

    async fn serve_unary(
        self,
        encoder: MkC::Encoder,
        decoder: MkC::Decoder,
        peer_addr: Option<volo::net::Address>,
        mode: UnaryMode,
    ) {
        if mode == UnaryMode::Explicit {
            self.serve_unary_inner(encoder, decoder, peer_addr, mode)
                .await;
            return;
        }
        let owner = Arc::new(crate::streaming::unary::ConnectionOwner::default());
        volo::streaming::ServerDrain::register(owner.clone());
        let guard = crate::streaming::unary::ConnectionGuard(owner.clone());
        owner
            .clone()
            .scope(Box::pin(
                self.serve_unary_inner(encoder, decoder, peer_addr, mode),
            ))
            .await;
        drop(guard);
        volo::streaming::NativeConnection::reclaimed(&*owner).await;
    }

    async fn serve_unary_inner(
        self,
        encoder: MkC::Encoder,
        decoder: MkC::Decoder,
        peer_addr: Option<volo::net::Address>,
        mode: UnaryMode,
    ) {
        #[cfg(not(any(feature = "multiplex", feature = "shmipc")))]
        let _ = mode;
        #[cfg(feature = "shmipc")]
        let _guard = (mode == UnaryMode::Explicit)
            .then(|| crate::codec::Decoder::shmipc_helper(&decoder).close_guard());
        #[cfg(feature = "multiplex")]
        if self.multiplex && mode != UnaryMode::Serial {
            #[cfg(feature = "shmipc")]
            if mode == UnaryMode::Explicit
                && peer_addr
                    .as_ref()
                    .is_some_and(volo::net::Address::is_shmipc)
            {
                tracing::error!("multiplex is not supported when using shmipc");
                let _ = crate::codec::Decoder::shmipc_helper(&decoder).close().await;
                return;
            }
            crate::transport::multiplex::serve_with_mode(
                encoder,
                decoder,
                self.control.idle_closed(),
                self.control.exit_mark.clone(),
                self.service,
                self.stat_tracer,
                peer_addr,
                mode == UnaryMode::Detected,
                self.unsafe_codec,
            )
            .await;
            return;
        }
        crate::transport::pingpong::serve_with_codec(
            encoder,
            decoder,
            self.control.idle_closed(),
            self.control.exit_mark.clone(),
            &self.service,
            self.stat_tracer,
            peer_addr,
            self.span_provider,
            self.unsafe_codec,
        )
        .await;
    }

    /// Check this before creating a mixed listener. Explicit servers do not need it.
    pub fn validate_buffered_handoff(&self) -> io::Result<()> {
        if !self.make_codec.supports_buffered() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "custom MakeCodec must support a prebuffered connection",
            ));
        }
        Ok(())
    }

    /// Consumes a classified, once-split connection without probing it again.
    /// The caller owns its connection task and any ShmIPC close guard from before detection.
    /// PingPong is deliberately serial, including when the explicit server enables multiplex.
    pub async fn serve_detected(
        self,
        parts: BufferedConnParts,
        protocol: DetectedThrift,
    ) -> io::Result<()> {
        let connections = self.control.connections.clone();
        connections.fetch_add(1, Ordering::AcqRel);
        defer! { connections.fetch_sub(1, Ordering::AcqRel); }
        match protocol {
            DetectedThrift::PingPong | DetectedThrift::Unary => {
                self.validate_buffered_handoff()?;
                let (encoder, decoder) = self
                    .make_codec
                    .make_codec_from_buffered(parts.reader, parts.writer)?;
                let mode = if matches!(protocol, DetectedThrift::PingPong) {
                    UnaryMode::Serial
                } else {
                    UnaryMode::Detected
                };
                self.serve_unary(encoder, decoder, parts.info.peer_addr, mode)
                    .await;
            }
            DetectedThrift::Ttstream => {
                let streaming = self.streaming.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "no TTStream methods are registered",
                    )
                })?;
                let shutdown = super::streaming_shutdown(&streaming.exit, &self.control.exit_mark);
                crate::transport::ttstream::connection::Connection::serve(
                    parts.reader,
                    parts.writer,
                    streaming.config.clone(),
                    streaming.hooks.clone(),
                    streaming.router.clone(),
                    parts.info.peer_addr,
                    shutdown,
                )
                .await
                .map_err(io::Error::other)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closing_idle_reads_is_observed_before_and_after_waiter_registration() {
        for early in [true, false] {
            let control = ConnectionControl::default();
            if early {
                control.close_idle();
            }
            let closed = control.idle_closed();
            tokio::pin!(closed);
            if !early {
                assert!(futures::poll!(&mut closed).is_pending());
                control.close_idle();
            }
            assert!(futures::poll!(&mut closed).is_ready());
        }
    }
}
