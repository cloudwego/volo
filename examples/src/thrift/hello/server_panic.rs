use std::net::SocketAddr;

use volo_thrift::server::panic_handler::log_and_return_exception;

pub struct S;

impl volo_gen::thrift_gen::hello::HelloService for S {
    async fn hello(
        &self,
        _req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        panic!("panic in hello");
    }

    async fn hello2(
        &self,
        _type: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        panic!("panic in hello");
    }

    async fn hello3(
        &self,
        _self: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        panic!("panic in hello");
    }
}

#[volo::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let addr: SocketAddr = "[::]:8081".parse().unwrap();
    let addr = volo::net::Address::from(addr);

    volo_gen::thrift_gen::hello::HelloServiceServer::new(S)
        .layer_front(volo::catch_panic::Layer::new(log_and_return_exception))
        .run(addr)
        .await
        .unwrap();
}
