use std::{
    convert::Infallible,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use futures::future::BoxFuture;
use hyper::{
    body::{Body, Incoming as BodyIncoming},
    server::conn::http1,
};
use hyper_util::rt::TokioIo;
use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
    BoxError,
};
use scopeguard::defer;
use tokio::sync::Notify;
use tracing::{info, trace};
use volo::net::{conn::Conn, incoming::Incoming, Address, MakeIncoming};

use crate::{
    context::ServerContext,
    request::Request,
    response::{IntoResponse, Response},
};

/// This is unstable now and may be changed in the future.
#[doc(hidden)]
type TraceFn = fn(&ServerContext);

pub struct Server<S, L> {
    service: S,
    layer: L,
    stat_tracer: Vec<TraceFn>,
    shutdown_hooks: Vec<Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>>,
}

impl<S> Server<S, Identity> {
    pub fn new(service: S) -> Self
    where
        S: Service<ServerContext, BodyIncoming, Response = Response, Error = Infallible>,
    {
        Self {
            service,
            layer: Identity::new(),
            stat_tracer: Vec::new(),
            shutdown_hooks: Vec::new(),
        }
    }
}

impl<S, L> Server<S, L> {
    /// Register shutdown hook.
    ///
    /// Hook functions will be called just before volo's own gracefull existing code starts,
    /// in reverse order of registration.
    pub fn register_shutdown_hook(
        mut self,
        hook: impl FnOnce() -> BoxFuture<'static, ()> + 'static + Send,
    ) -> Self {
        self.shutdown_hooks.push(Box::new(hook));
        self
    }

    /// Adds a new inner layer to the server.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer(baz)`, we will get: foo -> bar -> baz.
    pub fn layer<Inner>(self, layer: Inner) -> Server<S, Stack<Inner, L>> {
        Server {
            service: self.service,
            layer: Stack::new(layer, self.layer),
            stat_tracer: self.stat_tracer,
            shutdown_hooks: self.shutdown_hooks,
        }
    }

    /// Adds a new front layer to the server.
    ///
    /// The layer's `Service` should be `Send + Sync + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_front(baz)`, we will get: baz -> foo -> bar.
    pub fn layer_front<Front>(self, layer: Front) -> Server<S, Stack<L, Front>> {
        Server {
            service: self.service,
            layer: Stack::new(self.layer, layer),
            stat_tracer: self.stat_tracer,
            shutdown_hooks: self.shutdown_hooks,
        }
    }

    /// This is unstable now and may be changed in the future.
    #[doc(hidden)]
    pub fn stat_tracer(mut self, trace_fn: TraceFn) -> Self {
        self.stat_tracer.push(trace_fn);
        self
    }

    pub async fn run<MI: MakeIncoming>(self, mk_incoming: MI) -> Result<(), BoxError>
    where
        S: Service<ServerContext, BodyIncoming, Response = Response, Error = Infallible>,
        L: Layer<S>,
        L::Service: Service<ServerContext, BodyIncoming, Response = Response, Error = Infallible>
            + Send
            + Sync
            + 'static,
    {
        // init server
        let service = Arc::new(self.layer.layer(self.service));
        // TODO(lyf1999): type annotation is needed here, figure out why
        let stat_tracer: Arc<[TraceFn]> = Arc::from(self.stat_tracer);

        let mut incoming = mk_incoming.make_incoming().await?;
        info!("[VOLO] server start at: {:?}", incoming);

        let conn_cnt = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gconn_cnt = conn_cnt.clone();
        let (exit_notify, exit_flag) = (
            Arc::new(Notify::const_new()),
            Arc::new(parking_lot::RwLock::new(false)),
        );
        let (exit_notify_inner, exit_flag_inner) = (exit_notify.clone(), exit_flag.clone());

        // spawn accept loop
        let handler = tokio::spawn(async move {
            let exit_flag = exit_flag_inner.clone();
            loop {
                if *exit_flag.read() {
                    break Ok(());
                }
                match incoming.accept().await {
                    Ok(Some(conn)) => {
                        let peer = conn
                            .info
                            .peer_addr
                            .clone()
                            .expect("http address should have one");

                        trace!("[VOLO] accept connection from: {:?}", peer);

                        tokio::task::spawn(handle_conn(
                            conn,
                            service.clone(),
                            stat_tracer.clone(),
                            exit_notify_inner.clone(),
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

        if !self.shutdown_hooks.is_empty() {
            info!("[VOLO] call shutdown hooks");

            for hook in self.shutdown_hooks {
                (hook)().await;
            }
        }

        // received signal, graceful shutdown now
        info!("[VOLO] received signal, gracefully exiting now");
        *exit_flag.write() = true;

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
    stat_tracer: Arc<[TraceFn]>,
    exit_notify: Arc<Notify>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
    peer: Address,
) where
    S: Service<ServerContext, BodyIncoming, Response = Response, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
{
    conn_cnt.fetch_add(1, Ordering::Relaxed);
    defer! {
        conn_cnt.fetch_sub(1, Ordering::Relaxed);
    }
    let notified = exit_notify.notified();
    tokio::pin!(notified);

    let mut http_conn = http1::Builder::new().serve_connection(
        TokioIo::new(conn),
        hyper::service::service_fn(|req| {
            serve(service.clone(), peer.clone(), stat_tracer.clone(), req)
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
}

async fn serve<S>(
    service: S,
    peer: Address,
    stat_tracer: Arc<[TraceFn]>,
    request: Request,
) -> Result<Response, Infallible>
where
    S: Service<ServerContext, BodyIncoming, Response = Response, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
{
    let service = service.clone();
    let peer = peer.clone();
    let (parts, req) = request.into_parts();
    let mut cx = ServerContext::new(peer, parts);

    if let Some(req_size) = req.size_hint().exact() {
        cx.common_stats.set_req_size(req_size);
    }
    cx.stats.record_process_start_at();

    let resp = service.call(&mut cx, req).await.into_response();

    cx.stats.record_process_end_at();
    cx.stats.set_status_code(resp.status());
    if let Some(resp_size) = resp.size_hint().exact() {
        cx.common_stats.set_resp_size(resp_size);
    }

    stat_tracer.iter().for_each(|f| f(&cx));
    Ok(resp)
}
