//! Application send/receive order must respect both queue and H2 flow control.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use thrift_grpc_codegen::{
    fixture::Handler,
    volo_gen::grpc_thrift::{Empty, PureStreamingClientBuilder, PureStreamingServer},
};
use volo::net::incoming::DefaultIncoming;
use volo_grpc::{
    Code, Status,
    server::{Server, ServiceBuilder},
    streaming::StreamContext,
    thrift::channel,
};

#[tokio::test]
async fn sequential_send_then_receive_stalls_and_concurrent_driving_recovers() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            Server::new()
                .http2_init_stream_window_size(1024u32)
                .http2_init_connection_window_size(1024u32)
                .http2_max_send_buf_size(1024usize)
                .add_service(ServiceBuilder::new(PureStreamingServer::new(Handler)).build())
                .run_with_shutdown(DefaultIncoming::from(listener), async {
                    stopped.await.unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        let client = PureStreamingClientBuilder::new("application-flow")
            .address(address)
            .http2_init_stream_window_size(1024u32)
            .http2_init_connection_window_size(1024u32)
            .http2_max_send_buf_size(1024usize)
            .build();
        const MESSAGES: usize = 4096;
        let (mut tx, input) = channel::<Empty>(2);
        let response = client.bidi(input).await.unwrap();
        let cx = response
            .extensions()
            .get::<StreamContext>()
            .unwrap()
            .clone();
        let mut output = response.into_inner();
        let sent = Arc::new(AtomicUsize::new(0));
        let counter = sent.clone();
        let mut sequential = Box::pin(async {
            for _ in 0..MESSAGES {
                tx.send(&Empty {}).await?;
                counter.fetch_add(1, Ordering::Relaxed);
            }
            Ok::<_, Status>(())
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(250), sequential.as_mut())
                .await
                .is_err()
        );
        let stalled_at = sent.load(Ordering::Relaxed);
        assert!(
            stalled_at > 2 && stalled_at < MESSAGES,
            "the upload progresses before its bounded buffers fill"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), sequential.as_mut())
                .await
                .is_err()
        );
        assert_eq!(
            sent.load(Ordering::Relaxed),
            stalled_at,
            "a consumer must release the response window"
        );
        drop(sequential);
        cx.cancel();
        drop(tx);
        assert_eq!(output.message().await.unwrap_err().code(), Code::Cancelled);
        drop(output);
        cx.children_reclaimed().await;

        let (mut tx, input) = channel::<Empty>(2);
        let sending = async move {
            for _ in 0..MESSAGES {
                tx.send(&Empty {}).await?;
            }
            Ok::<_, Status>(())
        };
        let receiving = async {
            let mut output = client.bidi(input).await?.into_inner();
            let mut count = 0;
            while output.message().await?.is_some() {
                count += 1;
            }
            assert_eq!(count, MESSAGES);
            Ok::<_, Status>(())
        };
        futures::try_join!(sending, receiving).unwrap();
        drop(client);
        stop.send(()).unwrap();
        server.await.unwrap();
    })
    .await
    .expect("flow-control examples and cleanup are bounded");
}
