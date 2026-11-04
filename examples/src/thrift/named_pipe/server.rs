const PIPE_NAME: &str = r"\\.\pipe\volo_thrift_named_pipe_example";

pub struct S;

impl volo_gen::thrift_gen::hello::HelloService for S {
    async fn hello(
        &self,
        req: volo_gen::thrift_gen::hello::HelloRequest,
    ) -> Result<volo_gen::thrift_gen::hello::HelloResponse, volo_thrift::ServerError> {
        println!("req: {req:?}");
        Ok(volo_gen::thrift_gen::hello::HelloResponse {
            message: format!("Hello, {}!", req.name).into(),
            _field_mask: None,
        })
    }
}

#[volo::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let addr = volo::net::Address::from(PIPE_NAME);
    println!("listening on named pipe: {PIPE_NAME}");

    volo_gen::thrift_gen::hello::HelloServiceServer::new(S)
        .run(addr)
        .await
        .unwrap();
}
