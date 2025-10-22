use std::{net::SocketAddr, sync::LazyLock};

use pilota::FastStr;

static CLIENT: LazyLock<volo_gen::proto_gen::nested::GreeterClient> = LazyLock::new(|| {
    let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
    volo_gen::proto_gen::nested::GreeterClientBuilder::new("hello")
        .address(addr)
        .build()
});

#[volo::main]
async fn main() {
    let req = volo_gen::proto_gen::nested::HelloRequest {
        name: FastStr::from_static_str("Volo"),
        contact_info: Some(
            volo_gen::proto_gen::nested::hello_request::ContactInfo::Email(
                FastStr::from_static_str("volo@bytedance.com"),
            ),
        ),
    };

    let req_desc = volo_gen::proto_gen::nested::HelloRequest::get_descriptor_proto();
    println!("message descriptor: {req_desc:#?}\n");
    let user_desc = volo_gen::proto_gen::nested::hello_request::User::get_descriptor_proto();
    println!("nested message descriptor: {user_desc:#?}\n");
    let contact_info_desc =
        volo_gen::proto_gen::nested::hello_request::ContactInfo::get_descriptor_proto();
    println!("oneof descriptor: {contact_info_desc:#?}\n");
    let gender_desc = volo_gen::proto_gen::nested::hello_request::Gender::get_descriptor_proto();
    println!("nested enum descriptor: {gender_desc:#?}\n");

    let resp = CLIENT.say_hello(req).await;
    match resp {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("{e:?}"),
    }
}
