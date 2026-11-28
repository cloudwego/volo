use std::time::Duration;

use futures::StreamExt;
use streamx_codegen::{name_edges_grpc as grpc, name_edges_pb as pb, name_edges_thrift as thrift};
use thrift::{name_parent as parent, name_rust as renamed};
use volo::net::incoming::DefaultIncoming;
use volo_streamx::{RequestStream, ResponseSender, RpcResult, StreamContext};

struct Handler;

macro_rules! handler {
    ($binding:path) => {
        impl $binding for Handler {
            async fn inherited(
                &self,
                cx: StreamContext,
                request: parent::Message,
                mut output: ResponseSender<parent::Message>,
            ) -> RpcResult<()> {
                assert_eq!((cx.service(), cx.method()), ("Original", "Inherited"));
                output.send(&request).await
            }
            async fn fallible(
                &self,
                cx: StreamContext,
                request: parent::Message,
            ) -> RpcResult<Result<parent::Message, parent::BaseFallibleException>> {
                assert_eq!((cx.service(), cx.method()), ("Original", "Fallible"));
                Ok(Err(parent::BaseFallibleException::Failure(
                    parent::Failure {
                        reason: Some(request.text),
                    },
                )))
            }
            async fn copy(
                &self,
                _: StreamContext,
                request: renamed::Alias,
            ) -> RpcResult<renamed::Alias> {
                Ok(request)
            }
            async fn r#match(&self, _: StreamContext, _: ()) -> RpcResult<()> {
                Ok(())
            }
            async fn handle_original(
                &self,
                cx: StreamContext,
                mut input: RequestStream<parent::Message>,
            ) -> RpcResult<parent::Message> {
                assert_eq!((cx.service(), cx.method()), ("Original", "OriginalCase"));
                let request = input.recv().await?.unwrap();
                assert!(input.recv().await?.is_none());
                Ok(request)
            }
        }
    };
}
handler!(renamed::rewritten_streamx::Handler);
handler!(grpc::name_rust::rewritten_streamx::Handler);

#[test]
fn descriptors_preserve_idl_wire_names_despite_rust_renames() {
    for (descriptors, name, wire_name, payload, methods) in [
        (
            &[
                &thrift::hyphen::hyphen_streamx::DESCRIPTOR,
                &grpc::hyphen::hyphen_streamx::DESCRIPTOR,
            ][..],
            "Hyphen",
            "my_service.Hyphen",
            "Thrift",
            &[("Ping", "Unary")][..],
        ),
        (
            &[
                &thrift::no_namespace::global_streamx::DESCRIPTOR,
                &grpc::no_namespace::global_streamx::DESCRIPTOR,
            ][..],
            "Global",
            "no_namespace.Global",
            "Thrift",
            &[("Unary", "Unary")][..],
        ),
        (
            &[&pb::global_proto_streamx::DESCRIPTOR][..],
            "GlobalProto",
            "GlobalProto",
            "Protobuf",
            &[("Watch", "ServerStreaming"), ("match", "Unary")][..],
        ),
        (
            &[
                &parent::base_streamx::DESCRIPTOR,
                &grpc::name_parent::base_streamx::DESCRIPTOR,
            ][..],
            "Parent",
            "parent.Parent",
            "Thrift",
            &[("Fallible", "Unary"), ("Inherited", "ServerStreaming")][..],
        ),
        (
            &[
                &renamed::rewritten_streamx::DESCRIPTOR,
                &grpc::name_rust::rewritten_streamx::DESCRIPTOR,
            ][..],
            "Original",
            "renamed.Original",
            "Thrift",
            &[
                ("Copy", "Unary"),
                ("Fallible", "Unary"),
                ("Inherited", "ServerStreaming"),
                ("OriginalCase", "ClientStreaming"),
                ("match", "Unary"),
            ][..],
        ),
    ] {
        for descriptor in descriptors {
            assert_eq!((descriptor.name, descriptor.wire_name), (name, wire_name));
            assert_eq!(format!("{:?}", descriptor.payload), payload);
            assert_eq!(descriptor.methods.len(), methods.len());
            for (name, mode) in methods {
                let method = descriptor.methods.iter().find(|m| m.name == *name).unwrap();
                assert_eq!(format!("{:?}", method.mode), *mode);
                assert!(!method.oneway);
            }
        }
    }
}

#[tokio::test]
async fn renamed_parent_and_child_use_original_wire_names_and_shared_exception_types() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for grpc_binding in [false, true] {
            let server = volo_streamx::Server::new();
            let server = if grpc_binding {
                server.add_service(grpc::name_rust::rewritten_streamx::bind(Handler))
            } else {
                server.add_service(renamed::rewritten_streamx::bind(Handler))
            };
            let prepared = server
                .graceful_shutdown_timeout(Duration::from_millis(500))
                .prepare()
                .unwrap();
            let handle = prepared.handle();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(prepared.run(DefaultIncoming::from(listener)));
            let native = renamed::RewrittenClientBuilder::new("renamed")
                .address(address)
                .build();
            let grpc = grpc::name_rust::RewrittenClientBuilder::new("renamed")
                .address(address)
                .build();
            let request = parent::Message {
                text: "renamed".into(),
            };
            let alias = renamed::Alias(request.clone());
            assert_eq!(native.copy(alias.clone()).await.unwrap(), alias);
            assert_eq!(
                grpc.copy(grpc::name_rust::RewrittenCopyRequest {
                    request: alias.clone()
                })
                .await
                .unwrap()
                .into_inner(),
                alias
            );
            native.r#match().await.unwrap();
            grpc.r#match(grpc::name_rust::RewrittenMatchRequest {})
                .await
                .unwrap();
            let expected = parent::BaseFallibleException::Failure(parent::Failure {
                reason: Some(request.text.clone()),
            });
            match native.fallible(request.clone()).await.unwrap() {
                volo_thrift::MaybeException::Exception(error) => assert_eq!(error, expected),
                volo_thrift::MaybeException::Ok(_) => panic!("missing inherited exception"),
            }
            assert_eq!(
                grpc.fallible(grpc::name_parent::BaseFallibleRequest {
                    request: request.clone()
                })
                .await
                .unwrap()
                .into_inner()
                .unwrap_err(),
                expected
            );
            let mut output = native.inherited(request.clone()).await.unwrap();
            assert_eq!(output.recv().await.unwrap(), Some(request.clone()));
            assert!(output.recv().await.unwrap().is_none());
            let mut output = grpc.inherited(request.clone()).await.unwrap().into_inner();
            assert_eq!(output.next().await.unwrap().unwrap(), request);
            assert!(output.next().await.is_none());
            let mut input = native.handle_original().await.unwrap();
            input.send(&request).await.unwrap();
            assert_eq!(input.close_and_recv().await.unwrap(), request);
            assert_eq!(
                grpc.handle_original(futures::stream::iter([request.clone()]))
                    .await
                    .unwrap()
                    .into_inner(),
                request
            );
            handle.shutdown();
            server.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}
