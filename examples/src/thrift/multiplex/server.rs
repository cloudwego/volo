use std::net::SocketAddr;

pub struct S;

impl volo_gen::thrift_gen::hello::HelloService for S {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        let resp = volo_gen::thrift_gen::hello::HelloResponse {
            message: format!("Hello, {}!", req.name).into(),
        };
        Ok(resp)
    }

    async fn hello2(
        &self,
        _req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        panic!("panic in hello");
    }

    async fn hello3(
        &self,
        _req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        panic!("panic in hello");
    }
}

#[volo::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let addr: SocketAddr = "[::]:8081".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    volo_gen::thrift_gen::hello::HelloServiceServer::new(S)
        .multiplex(true)
        .run(addr)
        .await
        .unwrap();
}
