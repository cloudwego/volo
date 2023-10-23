use std::net::SocketAddr;

pub struct S;

impl volo_gen::thrift_gen::echo_unknown::EchoService for S {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::echo_unknown::EchoRequest,
    ) -> Result<volo_gen::thrift_gen::echo_unknown::EchoResponse, volo_thrift::AnyhowError> {
        let resp = volo_gen::thrift_gen::echo_unknown::EchoResponse {
            name: format!("{}", req.name).into(),
            echo_union: req.echo_union,
            _unknown_fields: req._unknown_fields,
        };
        Ok(resp)
    }
}

#[volo::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let addr: SocketAddr = "[::]:8081".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    volo_gen::thrift_gen::echo_unknown::EchoServiceServer::new(S)
        .run(addr)
        .await
        .unwrap();
}
