use std::{io, marker::PhantomData};

use motore::service::{Service, UnaryService};
use volo::net::{Address, dial::MakeTransport};

use crate::{
    EntryMessage, ThriftMessage,
    codec::MakeCodec,
    context::ClientContext,
    protocol::TMessageType,
    transport::{
        dial::TransportAcquirer,
        pingpong::thrift_transport::ThriftTransport,
        pool::{Config, Ver},
    },
};

#[derive(Clone)]
pub struct MakeClientTransport<MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf>,
{
    make_transport: MkT,
    make_codec: MkC,
}

impl<MkT, MkC> MakeClientTransport<MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf>,
{
    #[allow(unused)]
    #[inline]
    pub fn new(make_transport: MkT, make_codec: MkC) -> Self {
        Self {
            make_transport,
            make_codec,
        }
    }
}

impl<MkT, MkC> UnaryService<Address> for MakeClientTransport<MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    type Response = ThriftTransport<MkC::Encoder, MkC::Decoder>;
    type Error = io::Error;

    #[inline]
    async fn call(&self, target: Address) -> Result<Self::Response, Self::Error> {
        let make_transport = self.make_transport.clone();
        let (rh, wh) = make_transport.make_transport(target).await?;
        Ok(ThriftTransport::new(rh, wh, self.make_codec.clone()))
    }
}

pub struct Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    acquirer: TransportAcquirer<MakeClientTransport<MkT, MkC>>,
    _marker: PhantomData<Resp>,
}

impl<Resp, MkT, MkC> Clone for Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    fn clone(&self) -> Self {
        Self {
            acquirer: self.acquirer.clone(),
            _marker: self._marker,
        }
    }
}

impl<Resp, MkT, MkC> Client<Resp, MkT, MkC>
where
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    pub fn new(make_transport: MkT, pool_cfg: Option<Config>, make_codec: MkC) -> Self {
        let make_transport = MakeClientTransport::new(make_transport, make_codec);
        let acquirer = TransportAcquirer::new(make_transport, pool_cfg);
        Client {
            acquirer,
            _marker: PhantomData,
        }
    }
}

impl<Req, Resp, MkT, MkC> Service<ClientContext, ThriftMessage<Req>> for Client<Resp, MkT, MkC>
where
    Req: Send + 'static + EntryMessage,
    Resp: EntryMessage + Sync,
    MkT: MakeTransport,
    MkC: MakeCodec<MkT::ReadHalf, MkT::WriteHalf> + Sync,
{
    type Response = Option<ThriftMessage<Resp>>;

    type Error = crate::ClientError;

    #[inline]
    async fn call(
        &self,
        cx: &mut ClientContext,
        req: ThriftMessage<Req>,
    ) -> Result<Self::Response, Self::Error> {
        let oneway = cx.message_type == TMessageType::OneWay;
        // Acquire happens before `send`; the candidate walk and stats live in the acquirer.
        let mut acquired = self.acquirer.acquire(cx, Ver::PingPong).await?;
        // `send` may be cancelled by an outer timeout. Keep a clone of the shmipc stream in a
        // guard so cancellation closes it instead of leaving it in the session map forever.
        #[cfg(feature = "shmipc")]
        let mut shmipc_close_guard = {
            let helper = acquired.transport().shmipc_helper();
            helper.available().then(|| helper.close_guard())
        };
        let resp = acquired.transport_mut().send(cx, req, oneway).await;
        if let Ok(None) = resp {
            if !oneway {
                return Err(crate::ClientError::Transport(
                    pilota::thrift::TransportException::from(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "an unexpected end of file from server, rpc_info: {:?}",
                            cx.rpc_info
                        ),
                    )),
                ));
            }
        }
        // Shmipc manages its own stream pool; recycle only completed calls.
        #[cfg(feature = "shmipc")]
        {
            let helper = acquired.transport().shmipc_helper();
            if helper.available() {
                // A failed send can leave data in shmipc's send buffer (for example after a
                // persistent QueueFull). Only a completed RPC is safe to recycle; on error the
                // armed guard closes the stream and reclaims its buffers.
                if resp.is_ok() {
                    helper.reuse().await;
                    if let Some(guard) = shmipc_close_guard.take() {
                        guard.disarm();
                    }
                }
            } else if cx.transport.should_reuse && resp.is_ok() {
                acquired.reuse().await;
            }
        }
        #[cfg(not(feature = "shmipc"))]
        if cx.transport.should_reuse && resp.is_ok() {
            acquired.reuse().await;
        }

        resp
    }
}

#[cfg(all(test, feature = "shmipc", target_os = "linux"))]
mod tests {
    use std::{
        cell::RefCell,
        io,
        os::unix::net::SocketAddr,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use motore::service::{Service, UnaryService};
    use pilota::thrift::ThriftException;
    use tokio::{
        io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
        time::timeout,
    };
    use volo::{
        context::{Context, Role, RpcInfo},
        net::{
            Address,
            conn::{OwnedReadHalf, OwnedWriteHalf},
            dial::MakeTransport,
            ext::AsyncExt,
            shmipc::{Listener, addr::ShmipcMakeTransport},
        },
    };

    use super::Client;
    use crate::{
        Bytes, EntryMessage, ThriftMessage,
        codec::{Decoder, DefaultMakeCodec, Encoder, MakeCodec},
        context::{ClientContext, Config, ThriftContext},
        protocol::TMessageType,
    };

    #[derive(Clone)]
    struct OneShotTransport {
        halves: Arc<Mutex<Option<(OwnedReadHalf, OwnedWriteHalf)>>>,
    }

    impl MakeTransport for OneShotTransport {
        type ReadHalf = OwnedReadHalf;
        type WriteHalf = OwnedWriteHalf;

        async fn make_transport(
            &self,
            _addr: Address,
        ) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
            self.halves.lock().unwrap().take().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "transport already taken")
            })
        }

        fn set_connect_timeout(&mut self, _timeout: Option<Duration>) {}

        fn set_read_timeout(&mut self, _timeout: Option<Duration>) {}

        fn set_write_timeout(&mut self, _timeout: Option<Duration>) {}
    }

    struct SocketCleanup(PathBuf);

    impl Drop for SocketCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[derive(Clone)]
    struct WriteThenFailMakeCodec;

    struct WriteThenFailEncoder<W> {
        writer: W,
    }

    impl<W> Encoder for WriteThenFailEncoder<W>
    where
        W: AsyncWrite + AsyncExt + Unpin + Send + Sync + 'static,
    {
        async fn encode<Req: Send + EntryMessage, Cx: ThriftContext>(
            &mut self,
            _cx: &mut Cx,
            _msg: ThriftMessage<Req>,
        ) -> Result<(), ThriftException> {
            self.writer.write_all(b"buffered but unsent").await?;
            Err(io::Error::other("injected send failure").into())
        }

        fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
            self.writer.shmipc_helper()
        }
    }

    struct UnreachableDecoder<R> {
        reader: R,
    }

    impl<R> Decoder for UnreachableDecoder<R>
    where
        R: AsyncRead + AsyncExt + Unpin + Send + Sync + 'static,
    {
        async fn decode<Msg: Send + EntryMessage, Cx: ThriftContext>(
            &mut self,
            _cx: &mut Cx,
        ) -> Result<Option<ThriftMessage<Msg>>, ThriftException> {
            unreachable!("send failure must bypass decode")
        }

        fn shmipc_helper(&self) -> volo::net::shmipc::ShmipcHelper {
            self.reader.shmipc_helper()
        }
    }

    impl<R, W> MakeCodec<R, W> for WriteThenFailMakeCodec
    where
        R: AsyncRead + AsyncExt + Unpin + Send + Sync + 'static,
        W: AsyncWrite + AsyncExt + Unpin + Send + Sync + 'static,
    {
        type Encoder = WriteThenFailEncoder<W>;
        type Decoder = UnreachableDecoder<R>;

        fn make_codec(&self, reader: R, writer: W) -> (Self::Encoder, Self::Decoder) {
            (
                WriteThenFailEncoder { writer },
                UnreachableDecoder { reader },
            )
        }
    }

    #[tokio::test]
    async fn failed_call_does_not_reuse_shmipc_stream_with_unsent_data() {
        let path = std::env::temp_dir().join(format!(
            "volo_failed_shmipc_call_{}.sock",
            std::process::id()
        ));
        let _cleanup = SocketCleanup(path.clone());
        let _ = std::fs::remove_file(&path);
        let shmipc_addr = volo::net::shmipc::Address::from(
            SocketAddr::from_pathname(&path).expect("valid socket path"),
        );
        let _listener = Listener::listen(shmipc_addr.clone(), None)
            .await
            .expect("listen on shmipc socket");

        let stream = ShmipcMakeTransport::new()
            .call(shmipc_addr.clone())
            .await
            .expect("connect shmipc stream");
        let failed_stream_addr = stream.peer_addr();
        let (read_half, write_half) = stream.into_split();
        let halves = Arc::new(Mutex::new(Some((
            OwnedReadHalf::Shmipc(read_half),
            OwnedWriteHalf::Shmipc(write_half),
        ))));
        let client: Client<Bytes, _, _> =
            Client::new(OneShotTransport { halves }, None, WriteThenFailMakeCodec);

        let mut cx = ClientContext::new(
            1,
            RpcInfo::<Config>::with_role(Role::Client),
            TMessageType::Call,
        );
        cx.rpc_info_mut()
            .callee_mut()
            .set_address(Address::Shmipc(shmipc_addr.clone()));
        let req = ThriftMessage::mk_client_msg(&cx, Bytes::new());

        let result = metainfo::METAINFO
            .scope(
                RefCell::new(metainfo::MetaInfo::default()),
                client.call(&mut cx, req),
            )
            .await;
        assert!(result.is_err(), "the injected send failure must propagate");

        let next_stream = ShmipcMakeTransport::new()
            .call(shmipc_addr)
            .await
            .expect("connect another shmipc stream");
        assert_ne!(
            next_stream.peer_addr(),
            failed_stream_addr,
            "a stream with unsent data must not return to the session pool"
        );
        next_stream
            .helper()
            .close()
            .await
            .expect("close replacement stream");
    }

    #[tokio::test]
    async fn cancelled_call_closes_shmipc_stream() {
        let path = std::env::temp_dir().join(format!(
            "volo_cancelled_shmipc_call_{}.sock",
            std::process::id()
        ));
        let _cleanup = SocketCleanup(path.clone());
        let _ = std::fs::remove_file(&path);
        let shmipc_addr = volo::net::shmipc::Address::from(
            SocketAddr::from_pathname(&path).expect("valid socket path"),
        );
        let mut listener = Listener::listen(shmipc_addr.clone(), None)
            .await
            .expect("listen on shmipc socket");
        let mut accept = tokio::spawn(async move { listener.accept().await });

        let stream = ShmipcMakeTransport::new()
            .call(shmipc_addr.clone())
            .await
            .expect("connect shmipc stream");
        let (read_half, write_half) = stream.into_split();
        let halves = Arc::new(Mutex::new(Some((
            OwnedReadHalf::Shmipc(read_half),
            OwnedWriteHalf::Shmipc(write_half),
        ))));
        let client: Client<Bytes, _, _> = Client::new(
            OneShotTransport { halves },
            None,
            DefaultMakeCodec::default(),
        );

        let mut cx = ClientContext::new(
            1,
            RpcInfo::<Config>::with_role(Role::Client),
            TMessageType::Call,
        );
        cx.rpc_info_mut()
            .callee_mut()
            .set_address(Address::Shmipc(shmipc_addr));
        let req = ThriftMessage::mk_client_msg(&cx, Bytes::new());

        let mut call = Box::pin(metainfo::METAINFO.scope(
            RefCell::new(metainfo::MetaInfo::default()),
            client.call(&mut cx, req),
        ));
        let mut peer = timeout(Duration::from_secs(1), async {
            tokio::select! {
                result = &mut call => panic!("RPC completed before cancellation: {result:?}"),
                accepted = &mut accept => accepted
                    .expect("accept task should not panic")
                    .expect("accept shmipc stream"),
            }
        })
        .await
        .expect("server should receive the request");

        assert!(
            timeout(Duration::ZERO, call).await.is_err(),
            "the RPC must still be waiting for a response when it is cancelled"
        );

        let mut request = Vec::new();
        let read_result = timeout(Duration::from_secs(1), peer.read_to_end(&mut request))
            .await
            .expect("cancelled client should close the stream");
        if let Err(err) = read_result {
            // Older shmipc releases report their EndOfStream marker as UnexpectedEof.
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        }
        assert!(
            !request.is_empty(),
            "server should receive the encoded request"
        );

        peer.helper().close().await.expect("close server stream");
    }
}
