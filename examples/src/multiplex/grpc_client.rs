#![feature(type_alias_impl_trait)]

use std::net::SocketAddr;

use lazy_static::lazy_static;

lazy_static! {
    static ref GREETER_CLIENT: volo_gen::proto_gen::hello::GreeterClient = {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::hello::GreeterClientBuilder::new("hello")
            .address(addr)
            .build()
    };
    static ref ECHO_CLIENT: volo_gen::proto_gen::echo::EchoClient = {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::echo::EchoClientBuilder::new("hello")
            .address(addr)
            .build()
    };
}

#[volo::main]
async fn main() {
    let req = volo_gen::proto_gen::hello::HelloRequest {
        name: "Volo".to_string(),
    };
    let resp = GREETER_CLIENT.clone().say_hello(req).await;
    match resp {
        Ok(info) => println!("GREETER: {info:?}"),
        Err(e) => eprintln!("GREETER: {e:?}"),
    }

    let req = volo_gen::proto_gen::echo::EchoRequest {
        message: "Volo".to_string(),
    };
    let resp = ECHO_CLIENT.clone().unary_echo(req).await;
    match resp {
        Ok(info) => println!("ECHO: {info:?}"),
        Err(e) => eprintln!("ECHO: {e:?}"),
    }
}
