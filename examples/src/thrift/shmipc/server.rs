pub struct S;

impl volo_gen::thrift_gen::hello::HelloService for S {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        println!("req: {req:?}");
        let resp = volo_gen::thrift_gen::hello::HelloResponse {
            message: format!("Hello, {}!", req.name).into(),
            _field_mask: None,
        };
        Ok(resp)
    }
}

#[volo::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let addr = std::os::unix::net::SocketAddr::from_pathname("/tmp/hello_test.sock").unwrap();
    let addr = volo::net::Address::from(volo::net::ShmipcAddr(addr));

    volo_gen::thrift_gen::hello::HelloServiceServer::new(S)
        .run(addr)
        .await
        .unwrap();
}
