use std::{net::SocketAddr, sync::LazyLock};

use pilota::FastStr;

static GREETER_CLIENT: LazyLock<volo_gen::proto_gen::helloworld::GreeterClient> =
    LazyLock::new(|| {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::helloworld::GreeterClientBuilder::new("hello")
            .address(addr)
            .build()
    });

static ECHO_CLIENT: LazyLock<volo_gen::proto_gen::echo::EchoClient> = LazyLock::new(|| {
    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    volo_gen::proto_gen::echo::EchoClientBuilder::new("echo")
        .address(addr)
        .build()
});

#[volo::main]
async fn main() {
    let req = volo_gen::proto_gen::helloworld::HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };
    let resp = GREETER_CLIENT.say_hello(req).await;
    match resp {
        Ok(info) => println!("GREETER: {info:?}"),
        Err(e) => eprintln!("GREETER: {e:?}"),
    }

    let req = volo_gen::proto_gen::echo::EchoRequest {
        message: FastStr::from_static_str("Volo"),
        _unknown_fields: Default::default(),
    };
    let resp = ECHO_CLIENT.echo(req).await;
    match resp {
        Ok(info) => println!("ECHO: {info:?}"),
        Err(e) => eprintln!("ECHO: {e:?}"),
    }
}
