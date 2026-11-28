//! Handlers and assertions for Rust loopback regression tests.

use futures::stream;
use volo::streaming::{RequestStream, ResponseSender, RpcResult, StreamContext};
use volo_grpc::{Request, Response, Status};

use crate::volo_gen::{grpc_parent::*, grpc_thrift::*};

pub struct Handler;

async fn server_replies(request: Message, mut output: ResponseSender<Message>) -> RpcResult<()> {
    for sequence in 0..3 {
        output
            .send(&Message {
                payload: request.payload.clone(),
                sequence: Some(sequence),
            })
            .await?;
    }
    Ok(())
}

impl Interop for Handler {
    async fn inherited(
        &self,
        _cx: StreamContext,
        request: Message,
        output: ResponseSender<Message>,
    ) -> RpcResult<()> {
        server_replies(request, output).await
    }

    async fn unary(
        &self,
        request: Request<InteropUnaryRequest>,
    ) -> Result<Response<Message>, Status> {
        Ok(Response::new(request.into_inner().request))
    }

    async fn explicit_unary(
        &self,
        request: Request<InteropExplicitUnaryRequest>,
    ) -> Result<Response<Message>, Status> {
        Ok(Response::new(request.into_inner().request))
    }

    async fn zero(&self, _: Request<InteropZeroRequest>) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn multi(&self, request: Request<InteropMultiRequest>) -> Result<Response<i64>, Status> {
        let request = request.into_inner();
        Ok(Response::new(i64::from(request.left) + request.right))
    }

    async fn container(
        &self,
        request: Request<InteropContainerRequest>,
    ) -> Result<Response<Vec<i64>>, Status> {
        Ok(Response::new(request.into_inner().values))
    }

    async fn throw(
        &self,
        request: Request<InteropThrowRequest>,
    ) -> Result<Response<Result<Message, InteropThrowException>>, Status> {
        let request = request.into_inner().request;
        if request.payload.as_deref() == Some(b"biz") {
            return Err(volo_grpc::thrift::BizStatus {
                code: 409,
                message: "失败：\"\\\n".into(),
                extra: [("中文\"\\".into(), "值\n\t\0".into())].into(),
            }
            .into_status());
        }
        Ok(Response::new(
            match request.payload.as_deref().unwrap_or_default() {
                b"failure" => Err(InteropThrowException::Failure(Failure {
                    message: Some("declared failure".into()),
                })),
                b"other" => Err(InteropThrowException::Other(Other { code: Some(7) })),
                _ => Ok(request),
            },
        ))
    }

    async fn void_throw(
        &self,
        _: Request<InteropVoidThrowRequest>,
    ) -> Result<Response<Result<(), InteropVoidThrowException>>, Status> {
        Ok(Response::new(Err(InteropVoidThrowException::Failure(
            Failure {
                message: Some("void failure".into()),
            },
        ))))
    }

    async fn client(
        &self,
        _cx: StreamContext,
        mut input: RequestStream<Message>,
    ) -> RpcResult<Message> {
        let mut payload = Vec::new();
        let mut count = 0;
        while let Some(message) = input.recv().await? {
            payload.extend_from_slice(message.payload.as_deref().unwrap_or_default());
            count += 1;
        }
        Ok(Message {
            payload: Some(payload.into()),
            sequence: Some(count),
        })
    }

    async fn server(
        &self,
        _cx: StreamContext,
        request: Message,
        output: ResponseSender<Message>,
    ) -> RpcResult<()> {
        server_replies(request, output).await
    }

    async fn bidi(
        &self,
        _cx: StreamContext,
        mut messages: RequestStream<Message>,
        mut output: ResponseSender<Message>,
    ) -> RpcResult<()> {
        while let Some(message) = messages.recv().await? {
            output.send(&message).await?;
        }
        Ok(())
    }
}

impl PureUnary for Handler {
    async fn echo(
        &self,
        request: Request<PureUnaryEchoRequest>,
    ) -> Result<Response<Empty>, Status> {
        Ok(Response::new(request.into_inner().request))
    }
}

impl PureStreaming for Handler {
    async fn bidi(
        &self,
        _cx: StreamContext,
        mut input: RequestStream<Empty>,
        mut output: ResponseSender<Empty>,
    ) -> RpcResult<()> {
        while let Some(message) = input.recv().await? {
            output.send(&message).await?;
        }
        Ok(())
    }
}

impl Parent for Handler {
    async fn inherited(
        &self,
        _cx: StreamContext,
        request: Message,
        output: ResponseSender<Message>,
    ) -> RpcResult<()> {
        server_replies(request, output).await
    }
}

/// Exercise the generated unary and streaming API using a single configured client.
pub async fn verify_client(client: &InteropClient) -> Result<Vec<&'static str>, Status> {
    let message = |value: &'static [u8]| Message {
        payload: Some(pilota::Bytes::from_static(value)),
        sequence: Some(7),
    };
    let mut passed = Vec::new();
    let first = client
        .unary(InteropUnaryRequest {
            request: message(b"raw\0payload"),
        })
        .await?;
    assert_eq!(first.into_inner(), message(b"raw\0payload"));
    passed.push("unary-args-result");
    assert_eq!(
        client
            .explicit_unary(InteropExplicitUnaryRequest {
                request: message(b"explicit")
            })
            .await?
            .into_inner(),
        message(b"explicit")
    );
    passed.push("explicit-unary");
    client.zero(InteropZeroRequest {}).await?;
    passed.push("zero-void-stop");
    assert_eq!(
        client
            .multi(InteropMultiRequest {
                left: -3,
                right: i64::MAX
            })
            .await?
            .into_inner(),
        i64::MAX - 3
    );
    passed.push("multiple-parameters-field-ids");
    assert_eq!(
        client
            .container(InteropContainerRequest {
                values: vec![i64::MIN, 0, i64::MAX]
            })
            .await?
            .into_inner(),
        vec![i64::MIN, 0, i64::MAX]
    );
    passed.push("container-result");
    assert!(
        matches!(client.throw(InteropThrowRequest { request: message(b"failure") }).await?.into_inner(), Err(InteropThrowException::Failure(Failure { message })) if message.as_deref() == Some("declared failure"))
    );
    assert!(matches!(
        client
            .throw(InteropThrowRequest {
                request: message(b"other")
            })
            .await?
            .into_inner(),
        Err(InteropThrowException::Other(Other { code: Some(7) }))
    ));
    assert_eq!(
        client
            .throw(InteropThrowRequest {
                request: message(b"success")
            })
            .await?
            .into_inner(),
        Ok(message(b"success"))
    );
    passed.push("multiple-declared-exceptions");
    assert!(
        matches!(client.void_throw(InteropVoidThrowRequest {}).await?.into_inner(), Err(InteropVoidThrowException::Failure(Failure { message })) if message.as_deref() == Some("void failure"))
    );
    passed.push("single-declared-void-exception");
    let status = client
        .throw(InteropThrowRequest {
            request: message(b"biz"),
        })
        .await
        .expect_err("business error");
    let biz = volo_grpc::thrift::BizStatus::from_status(&status)?.expect("biz-status metadata");
    assert_eq!(biz.code, 409);
    assert_eq!(biz.message, "失败：\"\\\n");
    assert_eq!(biz.extra, [("中文\"\\".into(), "值\n\t\0".into())].into());
    passed.push("biz-status-unicode-json");
    for source in [
        vec![],
        vec![message(b"a")],
        vec![message(b"a"), message(b"bc")],
    ] {
        let count = source.len() as i64;
        let expected = source
            .iter()
            .flat_map(|value| value.payload.as_deref().unwrap_or_default().iter().copied())
            .collect::<Vec<_>>();
        let response = client.client(stream::iter(source)).await?.into_inner();
        assert_eq!(
            response.payload.as_deref().unwrap_or_default(),
            expected.as_slice()
        );
        assert_eq!(response.sequence, Some(count));
    }
    passed.push("client-stream-zero-one-many");
    for inherited in [false, true] {
        let mut response = if inherited {
            client.inherited(message(b"server")).await?.into_inner()
        } else {
            client.server(message(b"server")).await?.into_inner()
        };
        for sequence in 0..3 {
            assert_eq!(
                response.message().await?.unwrap(),
                Message {
                    payload: Some(pilota::Bytes::from_static(b"server")),
                    sequence: Some(sequence)
                }
            );
        }
        assert!(response.message().await?.is_none());
    }
    passed.push("server-stream-and-inherited-path");
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    // Make the first request available while the transport waits for response headers.
    tx.send(message(b"before-half-close")).await.unwrap();
    let mut response = client
        .bidi(volo_grpc::codegen::ReceiverStream::new(rx))
        .await?
        .into_inner();
    assert_eq!(
        response.message().await?.unwrap(),
        message(b"before-half-close")
    );
    drop(tx);
    assert!(response.message().await?.is_none());
    passed.push("bidi-first-response-before-half-close");
    Ok(passed)
}
