use std::{
    marker::PhantomData,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use futures::stream::TryStreamExt as _;
use motore::{
    layer::{Identity, Layer, Stack},
    service::Service,
    BoxError,
};
use pilota::thrift::EntryMessage;
use tokio::sync::Notify;
use tracing::info;

use crate::{
    codec::{
        framed::Framed, tt_header, MakeServerDecoder, MakeServerEncoder, MkDecoder, MkEncoder,
    },
    context::ServerContext,
    Result, Size,
};

pub struct Server<S, L, Req, MkE, MkD> {
    service: S,
    layer: L,
    mk_encoder: MkE,
    mk_decoder: MkD,
    _marker: PhantomData<fn(Req)>,
}

impl<S, Req>
    Server<
        S,
        Identity,
        Req,
        MakeServerEncoder<tt_header::DefaultTTHeaderCodec>,
        MakeServerDecoder<tt_header::DefaultTTHeaderCodec>,
    >
{
    pub fn new(service: S) -> Self
    where
        S: Service<ServerContext, Req>,
    {
        Self {
            mk_encoder: MakeServerEncoder::new(tt_header::DefaultTTHeaderCodec),
            mk_decoder: MakeServerDecoder::new(tt_header::DefaultTTHeaderCodec),
            service,
            layer: Identity::new(),
            _marker: PhantomData,
        }
    }
}

impl<S, L, Req, MkE, MkD> Server<S, L, Req, MkE, MkD> {
    /// Adds a new inner layer to the server.
    ///
    /// The layer's `Service` should be `Send + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer(baz)`, we will get: foo -> bar -> baz.
    pub fn layer<Inner>(self, layer: Inner) -> Server<S, Stack<Inner, L>, Req, MkE, MkD> {
        Server {
            layer: Stack::new(layer, self.layer),
            service: self.service,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            _marker: PhantomData,
        }
    }

    /// Adds a new front layer to the server.
    ///
    /// The layer's `Service` should be `Send + Clone + 'static`.
    ///
    /// # Order
    ///
    /// Assume we already have two layers: foo and bar. We want to add a new layer baz.
    ///
    /// The current order is: foo -> bar (the request will come to foo first, and then bar).
    ///
    /// After we call `.layer_front(baz)`, we will get: baz -> foo -> bar.
    pub fn layer_front<Front>(self, layer: Front) -> Server<S, Stack<L, Front>, Req, MkE, MkD> {
        Server {
            layer: Stack::new(self.layer, layer),
            service: self.service,
            mk_encoder: self.mk_encoder,
            mk_decoder: self.mk_decoder,
            _marker: PhantomData,
        }
    }

    /// Set the TTHeader encoder to use for the server.
    ///
    /// This should not be used by most users, Volo has already provided a default encoder.
    /// This is only useful if you want to customize TTHeader protocol and use it together with
    /// a proxy (such as service mesh).
    ///
    /// If you only want to transform metadata across microservices, you can use [`metainfo`] to do
    /// this.
    #[doc(hidden)]
    pub fn tt_header_encoder<TTEncoder>(
        self,
        tt_encoder: TTEncoder,
    ) -> Server<S, L, Req, MakeServerEncoder<TTEncoder>, MkD> {
        Server {
            layer: self.layer,
            service: self.service,
            mk_encoder: MakeServerEncoder::new(tt_encoder),
            mk_decoder: self.mk_decoder,
            _marker: PhantomData,
        }
    }

    /// Set the TTHeader decoder to use for the server.
    ///
    /// This should not be used by most users, Volo has already provided a default decoder.
    /// This is only useful if you want to customize TTHeader protocol and use it together with
    /// a proxy (such as service mesh).
    ///
    /// If you only want to transform metadata across microservices, you can use [`metainfo`] to do
    /// this.
    #[doc(hidden)]
    pub fn tt_header_decoder<TTDecoder>(
        self,
        tt_decoder: TTDecoder,
    ) -> Server<S, L, Req, MkE, MakeServerDecoder<TTDecoder>> {
        Server {
            layer: self.layer,
            service: self.service,
            mk_encoder: self.mk_encoder,
            mk_decoder: MakeServerDecoder::new(tt_decoder),
            _marker: PhantomData,
        }
    }

    /// The main entry point for the server.
    pub async fn run<A: volo::net::incoming::MakeIncoming, Resp>(
        self,
        incoming: A,
    ) -> Result<(), BoxError>
        where
            L: Layer<S>,
            MkE: MkEncoder,
            MkD: MkDecoder,
            L::Service: Service<ServerContext, Req, Response = Resp> + Clone + Send + 'static + Sync,
            <L::Service as Service<ServerContext, Req>>::Error: Into<BoxError> + Send,
            S: Service<ServerContext, Req, Response = Resp> + Clone + Send + 'static,
            S::Error: Into<BoxError>,
            Req: EntryMessage + Send + 'static,
            Resp: EntryMessage + Send + 'static + Size + Sync,
    {
        // init server
        let service = self.layer.layer(self.service);

        let mut incoming = incoming.make_incoming().await?;
        info!("[VOLO] server start at: {:?}", incoming);

        // graceful shutdown
        #[cfg(target_family = "unix")]
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        #[cfg(target_family = "unix")]
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        #[cfg(target_family = "unix")]
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

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
            loop {
                match incoming.try_next().await {
                    Ok(Some(conn)) => {
                        tracing::trace!("[VOLO] recv a connection from: {:?}", conn.info.peer_addr);
                        conn_cnt.fetch_add(1, Ordering::Relaxed);
                        let service = service.clone();

                        tokio::spawn(handle_conn(
                            conn,
                            service,
                            self.mk_encoder.clone(),
                            self.mk_decoder.clone(),
                            exit_notify_inner.clone(),
                            exit_flag_inner.clone(),
                            exit_mark_inner.clone(),
                            conn_cnt.clone(),
                        ));
                    }
                    // no more incoming connections
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                }
            }
        });

        // graceful shutdown handler
        #[cfg(target_family = "unix")]
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
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_conn<Req, Svc, Resp, MkE, MkD>(
    conn: volo::net::conn::Conn,
    service: Svc,
    mk_encoder: MkE,
    mk_decoder: MkD,
    exit_notify: Arc<Notify>,
    exit_flag: Arc<parking_lot::RwLock<bool>>,
    exit_mark: Arc<std::sync::atomic::AtomicBool>,
    conn_cnt: Arc<std::sync::atomic::AtomicUsize>,
) where
    MkE: MkEncoder,
    MkD: MkDecoder,
    Svc: Service<ServerContext, Req, Response = Resp> + Clone + Send + 'static,
    Svc::Error: Send,
    Svc::Error: Into<BoxError>,
    Req: EntryMessage + Send + 'static,
    Resp: EntryMessage + Send + 'static + Size,
{
    // get read lock and create Notified
    let notified = {
        let r = exit_flag.read();
        if *r {
            return;
        }
        exit_notify.notified()
    };

    let stream = conn.stream;
    let encoder = mk_encoder.mk_encoder(None);
    let decoder = mk_decoder.mk_decoder(None);

    let framed = Framed::new(stream, encoder, decoder);

    tracing::trace!("[VOLO] handle conn by pingpong");
    crate::transport::pingpong::serve(framed, notified, exit_mark, service).await;
    conn_cnt.fetch_sub(1, Ordering::Relaxed);
}
