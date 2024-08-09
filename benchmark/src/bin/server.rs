use std::net::SocketAddr;

use anyhow::anyhow;
use benchmark::{
    benchmark::echo::{EchoServer, ObjReq, ObjResp, Request, Response},
    perf::Recoder,
    runner::processor::process_request,
};
use lazy_static::lazy_static;
use volo_thrift::ServerError;

lazy_static! {
    static ref RECODER: Recoder = Recoder::new("VOLO@Server");
}

pub struct S;

impl EchoServer for S {
    async fn echo(&self, req: Request) -> Result<Response, ServerError> {
        let resp = process_request(&RECODER, req).await;
        Ok(resp)
    }

    async fn test_obj(&self, _req: ObjReq) -> Result<ObjResp, ServerError> {
        Err(anyhow!("not implemented").into())
    }
}

#[volo::main]
async fn main() {
    let addr: SocketAddr = "[::]:8001".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    benchmark::benchmark::echo::EchoServerServer::new(S)
        .run(addr)
        .await
        .unwrap();
}
