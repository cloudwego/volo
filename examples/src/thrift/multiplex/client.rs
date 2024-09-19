use std::{future, net::SocketAddr, sync::LazyLock};

use volo_thrift::client::CallOpt;

static CLIENT: LazyLock<volo_gen::thrift_gen::hello::HelloServiceClient> = LazyLock::new(|| {
    let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
    volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(addr)
        .multiplex(true)
        .build()
});

#[volo::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let futs = |i| async move {
        let req = volo_gen::thrift_gen::hello::HelloRequest {
            name: format!("volo{}", i).into(),
        };
        let resp = CLIENT
            .clone()
            .with_callopt(CallOpt::default())
            .hello(req)
            .await;
        match resp {
            Ok(info) => println!("{info:?}"),
            Err(e) => eprintln!("{e:?}"),
        }
    };

    let mut resps = Vec::with_capacity(10);
    for i in 0..resps.capacity() {
        resps.push(tokio::spawn(futs(i)));
    }

    for resp in resps {
        let _ = resp.await;
    }
    future::pending::<()>().await;
}
