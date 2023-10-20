use std::net::SocketAddr;

pub struct S;

#[volo::async_trait]
impl volo_gen::thrift_gen::hello::HelloService for S {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::AnyhowError> {
        let resp = volo_gen::thrift_gen::hello::HelloResponse {
            message: format!("Hello, {}!", req.name).into(),
        };
        Ok(resp)
    }
}

#[volo::main]
async fn main() {
    let addr: SocketAddr = "[::]:8081".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    volo_gen::thrift_gen::hello::HelloServiceServer::new(S)
        .run(addr)
        .await
        .unwrap();
}
