use std::{io, time::Duration};

use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::sync::watch;
use volo::net::conn::{Conn, PeekedConn};
#[cfg(feature = "__tls")]
use volo::net::tls::ServerTlsConfig;

use super::{Http2Config, incoming::IncomingService};
use crate::{Status, body::BoxBody};

/// Prepared layers and native connection configuration, without a listener.
#[doc(hidden)]
#[derive(Clone)]
pub struct PreparedServer<S> {
    pub(super) service: S,
    pub(super) managed_transport: bool,
    pub(super) http2_config: Http2Config,
    pub(super) shutdown_timeout: Duration,
    #[cfg(feature = "__tls")]
    pub(super) tls_config: Option<ServerTlsConfig>,
}

impl<S> PreparedServer<S> {
    pub fn uses_tls(&self) -> bool {
        #[cfg(feature = "__tls")]
        {
            self.tls_config.is_some()
        }
        #[cfg(not(feature = "__tls"))]
        {
            false
        }
    }

    /// Complete the shared TCP handshake once, preserving already negotiated sessions.
    pub fn accept_streamx_connection(
        &self,
        conn: Conn,
    ) -> impl std::future::Future<Output = io::Result<Conn>> + Send + 'static {
        #[cfg(not(feature = "__tls"))]
        {
            std::future::ready(Ok(conn))
        }
        #[cfg(feature = "__tls")]
        {
            let config = self.tls_config.clone();
            async move {
                if let Some(config) = config {
                    let Conn { stream, info } = conn;
                    let stream = match stream {
                        volo::net::conn::ConnStream::Tcp(tcp) => {
                            config.acceptor.accept(tcp).await?
                        }
                        stream => {
                            if !stream.is_tls() {
                                return Err(io::Error::new(
                                    io::ErrorKind::Unsupported,
                                    "StreamX TLS requires TCP",
                                ));
                            }
                            stream
                        }
                    };
                    return Ok(Conn { stream, info });
                }
                Ok(conn)
            }
        }
    }
    /// Validate before creating a StreamX listener, including single-driver servers.
    pub fn validate_streamx(&self) -> io::Result<()> {
        self.http2_config.validate_streamx()?;
        #[cfg(feature = "__tls")]
        if let Some(config) = &self.tls_config {
            config.acceptor.validate_streamx()?;
        }
        Ok(())
    }

    pub fn http2_config(&self) -> Http2Config {
        self.http2_config
    }

    pub fn shutdown_timeout(&self) -> Duration {
        self.shutdown_timeout
    }

    #[cfg(feature = "__tls")]
    pub fn tls_config(&self) -> Option<&ServerTlsConfig> {
        self.tls_config.as_ref()
    }
}

impl<S> PreparedServer<S>
where
    S: tower::Service<hyper::Request<BoxBody>, Response = hyper::Response<BoxBody>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Status> + Send + Sync + std::error::Error + 'static,
{
    /// Original explicit entry, including configured TLS and HTTP/1 support.
    pub async fn serve(self, conn: Conn, mut shutdown: watch::Receiver<()>) {
        #[cfg(feature = "__tls")]
        let conn = {
            let Conn { stream, info } = conn;
            match (
                stream,
                self.tls_config.as_ref().map(|config| &config.acceptor),
            ) {
                (volo::net::conn::ConnStream::Tcp(tcp), Some(acceptor)) => {
                    let result = tokio::select! {
                        result = acceptor.accept(tcp) => result,
                        _ = shutdown.changed() => return,
                    };
                    let stream = match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            tracing::debug!("[VOLO] TLS handshake error: {error}");
                            return;
                        }
                    };
                    Conn { stream, info }
                }
                (stream, _) => Conn { stream, info },
            }
        };
        let service = IncomingService::new(self.service, conn.info.peer_addr.clone());
        if self.managed_transport {
            super::h2::serve(conn, service, self.http2_config, shutdown).await;
            return;
        }
        let config = self.http2_config;
        let mut server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
        if !config.accept_http1 {
            server = server.http2_only();
        }
        server
            .http2()
            .initial_stream_window_size(config.init_stream_window_size)
            .timer(TokioTimer::new())
            .initial_connection_window_size(config.init_connection_window_size)
            .adaptive_window(config.adaptive_window)
            .max_concurrent_streams(config.max_concurrent_streams)
            .keep_alive_interval(config.http2_keepalive_interval)
            .keep_alive_timeout(config.http2_keepalive_timeout)
            .max_frame_size(config.max_frame_size)
            .max_send_buf_size(config.max_send_buf_size)
            .max_header_list_size(config.max_header_list_size);
        let mut connection = std::pin::pin!(server.serve_connection(
            TokioIo::new(conn),
            hyper::service::service_fn(move |request| {
                let mut service = service.clone();
                async move { tower::Service::call(&mut service, request).await }
            }),
        ));
        let mut draining = false;
        loop {
            tokio::select! {
                _ = shutdown.changed(), if !draining => {
                    draining = true;
                    connection.as_mut().graceful_shutdown();
                }
                result = &mut connection => {
                    if let Err(error) = result {
                        tracing::debug!("[VOLO] connection error: {error}");
                    }
                    break;
                }
            }
        }
    }

    /// The listener already completed TLS and classified this intact connection.
    /// This entry does not handshake TLS or identify HTTP/1 again.
    pub async fn serve_detected(
        self,
        io: PeekedConn,
        shutdown: watch::Receiver<()>,
    ) -> io::Result<()> {
        self.validate_streamx()?;
        super::h2::serve_detected(io, self.service, self.http2_config, shutdown).await
    }

    /// StreamX's H2-only entry: no probe or HTTP/1 identification is performed.
    pub async fn serve_selected(self, conn: Conn, shutdown: watch::Receiver<()>) -> io::Result<()> {
        self.validate_streamx()?;
        if conn
            .stream
            .negotiated_alpn()
            .is_some_and(|alpn| alpn != b"h2")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incompatible HTTP/2 ALPN",
            ));
        }
        let service =
            IncomingService::new(self.service, conn.info.peer_addr.clone()).with_strict_grpc();
        super::h2::serve_h2(conn, service, self.http2_config, shutdown).await;
        Ok(())
    }
}
