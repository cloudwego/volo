use std::time::Duration;

use thrift_grpc_codegen::{
    metadata_values::{MetadataValuesHandler, verify},
    volo_gen::grpc_thrift::{PureStreamingClientBuilder, PureStreamingServer},
};
use volo::net::{Address, incoming::DefaultIncoming};
use volo_grpc::server::{Server, ServiceBuilder};

#[tokio::test]
async fn repeated_and_opaque_metadata_reach_handler_header_and_trailer() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for unix in [false, true] {
            #[cfg(unix)]
            let directory = tempfile::tempdir().unwrap();
            let (incoming, address) = if unix {
                #[cfg(unix)]
                {
                    let path = directory.path().join("metadata.sock");
                    let listener = tokio::net::UnixListener::bind(&path).unwrap();
                    (
                        DefaultIncoming::from(listener),
                        Address::Unix(std::os::unix::net::SocketAddr::from_pathname(path).unwrap()),
                    )
                }
                #[cfg(not(unix))]
                continue;
            } else {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = Address::Ip(listener.local_addr().unwrap());
                (DefaultIncoming::from(listener), address)
            };
            let (shutdown, stopped) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                Server::new()
                    .add_service(
                        ServiceBuilder::new(PureStreamingServer::new(MetadataValuesHandler))
                            .build(),
                    )
                    .run_with_shutdown(incoming, async {
                        stopped.await.unwrap();
                        Ok(())
                    })
                    .await
                    .unwrap();
            });
            let client = PureStreamingClientBuilder::new("metadata")
                .address(address)
                .build();
            verify(&client).await.unwrap();
            drop(client);
            shutdown.send(()).unwrap();
            server.await.unwrap();
        }
    })
    .await
    .expect("metadata exchange and cleanup must complete");
}
