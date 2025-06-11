use std::{net::SocketAddr, sync::LazyLock, time::Duration};

use metainfo::{MetaInfo, METAINFO};
use pilota::FastStr;

static CLIENT: LazyLock<volo_gen::proto_gen::helloworld::GreeterClient> = LazyLock::new(|| {
    // Service A
    let addr: SocketAddr = "127.0.0.1:8081".parse().unwrap();
    volo_gen::proto_gen::helloworld::GreeterClientBuilder::new("hello-a")
        .address(addr)
        .rpc_timeout(Some(Duration::from_secs(3)))
        .build()
});

#[volo::main]
async fn main() {
    let mut mi = MetaInfo::new();
    mi.insert::<Duration>(Duration::from_secs(5));

    METAINFO
        .scope(mi.into(), async {
            let req = volo_gen::proto_gen::helloworld::HelloRequest {
                name: FastStr::from_static_str("Client"),
            };

            match CLIENT.say_hello(req).await {
                Ok(info) => println!("{info:?}"),
                Err(e) => eprintln!("{e:?}"),
            }
        })
        .await;
}
