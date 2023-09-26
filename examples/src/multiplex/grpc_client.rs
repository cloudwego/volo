#![feature(impl_trait_in_assoc_type)]

use std::net::SocketAddr;

use lazy_static::lazy_static;
use pilota::FastStr;

lazy_static! {
    static ref GREETER_CLIENT: volo_gen::proto_gen::hello::GreeterClient = {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::hello::GreeterClientBuilder::new("hello")
            .address(addr)
            .build()
    };
    static ref ECHO_CLIENT: volo_gen::proto_gen::echo::EchoClient = {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        volo_gen::proto_gen::echo::EchoClientBuilder::new("echo")
            .address(addr)
            .build()
    };
}

#[volo::main]
async fn main() {
    let req = volo_gen::proto_gen::hello::HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };
    let resp = GREETER_CLIENT.say_hello(req).await;
    match resp {
        Ok(info) => println!("GREETER: {info:?}"),
        Err(e) => eprintln!("GREETER: {e:?}"),
    }

    let req = volo_gen::proto_gen::echo::EchoRequest {
        message: FastStr::from_static_str("Volo"),
    };
    let resp = ECHO_CLIENT.echo(req).await;
    match resp {
        Ok(info) => println!("ECHO: {info:?}"),
        Err(e) => eprintln!("ECHO: {e:?}"),
    }
}
