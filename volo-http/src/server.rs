use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use http::Response;
use hyper::{
    body::{Body, Incoming as BodyIncoming},
    server::conn::http1,
};
use motore::BoxError;
use tracing::{info, trace};
use volo::net::{incoming::Incoming, MakeIncoming};

use crate::{DynError, HttpContext, MotoreService};

#[derive(Clone)]
pub struct Server<App> {
    app: App,
}

impl<OB, App> Server<App>
where
    OB: Body<Error = DynError> + Send + 'static,
    OB::Data: Send,
    App: motore::Service<HttpContext, BodyIncoming, Response = Response<OB>>
        + Clone
        + Send
        + Sync
        + 'static,
    App::Error: Into<DynError>,
{
    pub fn new(app: App) -> Self {
        Self { app: app }
    }

    pub async fn run<MI: MakeIncoming>(self, mk_incoming: MI) -> Result<(), BoxError> {
        let mut incoming = mk_incoming.make_incoming().await?;
        info!("[VOLO-HTTP] server start at: {:?}", incoming);

        let (tx, rx) = tokio::sync::watch::channel(());
        let exit_mark = Arc::new(std::sync::atomic::AtomicBool::default());

        let exit_mark_inner = exit_mark.clone();
        let rx_inner = rx.clone();

        let service = self;
        let handler = tokio::spawn(async move {
            let exit_mark = exit_mark_inner.clone();
            loop {
                if exit_mark.load(Ordering::Relaxed) {
                    break Ok(());
                }
                match incoming.accept().await {
                    Ok(Some(conn)) => {
                        let peer = conn.info.peer_addr.clone().unwrap();
                        trace!("[VOLO] accept connection from: {:?}", peer);

                        let s = service.clone();
                        let mut watch = rx_inner.clone();
                        tokio::task::spawn(async move {
                            let mut http_conn = http1::Builder::new()
                                .serve_connection(conn, MotoreService { peer, inner: s.app });
                            tokio::select! {
                                _ = watch.changed() => {
                                    tracing::trace!("[VOLO] closing a pending connection");
                                    // Graceful shutdown.
                                    hyper::server::conn::http1::Connection::graceful_shutdown(Pin::new(&mut http_conn));
                                    // Continue to poll this connection until shutdown can finish.
                                    let result = http_conn.await;
                                    if let Err(err) = result {
                                        tracing::debug!("[VOLO] connection error: {:?}", err);
                                    }
                                }
                                result = &mut http_conn => {
                                    if let Err(err) = result {
                                        tracing::debug!("[VOLO] connection error: {:?}", err);
                                    }
                                },
                            }
                        });
                    }
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(Box::new(e)),
                }
            }
        });

        #[cfg(target_family = "unix")]
        {
            // graceful shutdown
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
            let mut sighup =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

            // graceful shutdown handler
            tokio::select! {
                _ = sigint.recv() => {}
                _ = sighup.recv() => {}
                _ = sigterm.recv() => {}
                res = handler => {
                    match res {
                        Ok(res) => {
                            match res {
                                Ok(()) => {}
                                Err(e) => return Err(Box::new(e))
                            };
                        }
                        Err(e) => return Err(Box::new(e)),
                    }
                }
            }
        }

        // graceful shutdown handler for windows
        #[cfg(target_family = "windows")]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            res = handler => {
                match res {
                    Ok(res) => {
                        match res {
                            Ok(()) => {}
                            Err(e) => return Err(Box::new(e))
                        };
                    }
                    Err(e) => return Err(Box::new(e)),
                }
            }
        }

        // received signal, graceful shutdown now
        info!("[VOLO] received signal, gracefully exiting now");
        exit_mark.store(true, Ordering::Relaxed);
        drop(rx);
        let _ = tx.send(());
        let _ = tokio::time::timeout(Duration::from_secs(5), tx.closed()).await;
        Ok(())
    }
}
