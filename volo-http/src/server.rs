use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use hyper::{body::Incoming as BodyIncoming, server::conn::http1};
use hyper_util::rt::TokioIo;
use motore::BoxError;
use tokio::sync::Notify;
use tracing::{info, trace};
use volo::net::{conn::Conn, incoming::Incoming, Address, MakeIncoming};

use crate::{param::Params, response::Response, DynError, HttpContext};

pub struct Server<App> {
    app: Arc<App>,
}

impl<A> Clone for Server<A> {
    fn clone(&self) -> Self {
        Self {
            app: self.app.clone(),
        }
    }
}

impl<App> Server<App>
where
    App: motore::Service<HttpContext, BodyIncoming, Response = Response> + Send + Sync + 'static,
    App::Error: Into<DynError>,
{
    pub fn new(app: App) -> Self {
        Self { app: Arc::new(app) }
    }

    pub async fn run<MI: MakeIncoming>(self, mk_incoming: MI) -> Result<(), BoxError> {
        let mut incoming = mk_incoming.make_incoming().await?;
        info!("[VOLO] server start at: {:?}", incoming);

        let conn_cnt = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gconn_cnt = conn_cnt.clone();
        let (exit_notify, exit_flag, exit_mark) = (
            Arc::new(Notify::const_new()),
            Arc::new(parking_lot::RwLock::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::default()),
        );
        let (exit_notify_inner, exit_flag_inner, exit_mark_inner) =
            (exit_notify.clone(), exit_flag.clone(), exit_mark.clone());

        // spawn accept loop
        let handler = tokio::spawn(async move {
            let exit_flag = exit_flag_inner.clone();
            loop {
                if *exit_flag.read() {
                    break Ok(());
                }
                match incoming.accept().await {
                    Ok(Some(conn)) => {
                        let peer = conn.info.peer_addr.clone().unwrap();
                        trace!("[VOLO] accept connection from: {:?}", peer);
                        conn_cnt.fetch_add(1, Ordering::Relaxed);

                        tokio::task::spawn(handle_conn(
                            conn,
                            self.app.clone(),
                            exit_notify_inner.clone(),
                            exit_mark_inner.clone(),
                            conn_cnt.clone(),
                            peer,
                        ));
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
        *exit_flag.write() = true;
        exit_mark.store(true, Ordering::Relaxed);

        // Now we won't accept new connections.
        // And we want to send crrst reply to the peers in the short future.
        if gconn_cnt.load(Ordering::Relaxed) != 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        exit_notify.notify_waiters();

        // wait for all connections to be closed
        for _ in 0..28 {
            if gconn_cnt.load(Ordering::Relaxed) == 0 {
                break;
            }
            trace!(
                "[VOLO] gracefully exiting, remaining connection count: {}",
                gconn_cnt.load(Ordering::Relaxed)
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        Ok(())
    }
}

async fn handle_conn<S>(
    conn: Conn,
    service: S,
    exit_notify: Arc<Notify>,
    _exit_mark: Arc<std::sync::atomic::AtomicBool>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
    peer: Address,
) where
    S: motore::Service<HttpContext, BodyIncoming, Response = Response>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Error: Into<DynError>,
{
    let notified = exit_notify.notified();
    tokio::pin!(notified);

    let mut http_conn = http1::Builder::new().serve_connection(
        TokioIo::new(conn),
        hyper::service::service_fn(move |req: hyper::http::Request<BodyIncoming>| {
            let service = service.clone();
            let peer = peer.clone();
            async move {
                let (parts, req) = req.into_parts();
                let req = req.into();
                let mut cx = HttpContext {
                    peer,
                    method: parts.method,
                    uri: parts.uri,
                    version: parts.version,
                    headers: parts.headers,
                    extensions: parts.extensions,
                    params: Params {
                        inner: Vec::with_capacity(0),
                    },
                };
                service.call(&mut cx, req).await.map(|resp| resp.0)
            }
        }),
    );
    tokio::select! {
        _ = &mut notified => {
            tracing::trace!("[VOLO] closing a pending connection");
            // Graceful shutdown.
            hyper::server::conn::http1::Connection::graceful_shutdown(
                Pin::new(&mut http_conn)
            );
            // Continue to poll this connection until shutdown can finish.
            let result = http_conn.await;
            if let Err(err) = result {
                tracing::debug!("[VOLO] connection error: {:?}", err);
            }
        }
        result = &mut http_conn => {
            if let Err(err) = result {
                tracing::debug!("[VOLO] http connection error: {:?}", err);
            }
        },
    }
    conn_cnt.fetch_sub(1, Ordering::Relaxed);
}
