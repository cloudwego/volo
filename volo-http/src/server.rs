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
use tokio::sync::Notify;
use tracing::{info, trace, warn};
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

        let conn_cnt = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gconn_cnt = conn_cnt.clone();
        let (exit_notify, exit_flag, exit_mark) = (
            Arc::new(Notify::const_new()),
            Arc::new(parking_lot::RwLock::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::default()),
        );

        let exit_flag_inner = exit_flag.clone();

        let service = self;
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

                        let s = service.clone();
                        tokio::task::spawn(async move {
                            if let Err(err) = http1::Builder::new()
                                .serve_connection(conn, MotoreService { peer, inner: s.app })
                                .await
                            {
                                warn!("error serving connection: {:?}", err);
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
