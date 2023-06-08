#![feature(impl_trait_in_assoc_type)]

use std::net::SocketAddr;

use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use volo_gen::proto_gen::streaming::{StreamingRequest, StreamingResponse};
use volo_grpc::{
    server::{Server, ServiceBuilder},
    BoxStream, RecvStream, Request, Response, Status,
};

pub struct S;

#[volo::async_trait]
impl volo_gen::proto_gen::streaming::Streaming for S {
    async fn unary(
        &self,
        req: Request<StreamingRequest>,
    ) -> Result<Response<StreamingResponse>, Status> {
        let resp = StreamingResponse {
            message: format!("Unary, {}!", req.get_ref().message).into(),
        };
        Ok(volo_grpc::Response::new(resp))
    }

    async fn client_streaming(
        &self,
        req: Request<RecvStream<StreamingRequest>>,
    ) -> Result<Response<StreamingResponse>, Status> {
        let req = req.into_inner();
        let mut req = req.take(10);
        let mut resp = None;
        while let Some(result) = req.next().await {
            match result {
                Ok(req) => {
                    if resp.is_none() {
                        resp = Some(Ok(StreamingResponse {
                            message: format!("ClientStreaming, {}!", req.message).into(),
                        }));
                    }
                }
                Err(err) => {
                    eprintln!("client streaming poll request error: {err}");
                    resp = Some(Err(err));
                    break;
                }
            }
        }
        resp.unwrap().map(Response::new)
    }

    async fn server_streaming(
        &self,
        req: Request<StreamingRequest>,
    ) -> Result<Response<BoxStream<'static, Result<StreamingResponse, Status>>>, Status> {
        let req = req.into_inner();
        let repeat = std::iter::repeat(StreamingResponse {
            message: format!("ServerStreaming, {}!", req.message).into(),
        });
        let mut resp = tokio_stream::iter(repeat).take(10);
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(resp) = resp.next().await {
                match tx.send(Result::<_, Status>::Ok(resp)).await {
                    Ok(_) => {}
                    Err(_) => {
                        break;
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn bidirectional_streaming(
        &self,
        req: Request<RecvStream<StreamingRequest>>,
    ) -> Result<Response<BoxStream<'static, Result<StreamingResponse, Status>>>, Status> {
        let req = req.into_inner();
        let req = req.take(10);
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            tokio::pin!(req);
            while let Some(req) = req.next().await {
                match req {
                    Ok(req) => {
                        let resp = StreamingResponse {
                            message: format!("BidirectionalStreaming, {}!", req.message).into(),
                        };
                        match tx.send(Ok(resp)).await {
                            Ok(_) => {}
                            Err(err) => {
                                eprintln!("bidirectional_streaming send error: {err}");
                                break;
                            }
                        }
                    }
                    Err(e) => match tx.send(Err(e)).await {
                        Ok(_) => {}
                        Err(_) => break,
                    },
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

#[volo::main]
async fn main() {
    let addr: SocketAddr = "[::]:8080".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    Server::new()
        .add_service(
            ServiceBuilder::new(volo_gen::proto_gen::streaming::StreamingServer::new(S)).build(),
        )
        .run(addr)
        .await
        .unwrap();
}
