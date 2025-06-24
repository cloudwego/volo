// Ensure example.com is mapped to a local IP address (e.g. 127.0.0.1) before running this
// example by editing hosts file (e.g. /etc/hosts on Unix or
// C:\Windows\System32\drivers\etc\hosts on Windows)

use pilota::FastStr;
use volo_gen::proto_gen::helloworld::{GreeterClient, GreeterClientBuilder};

#[volo::main]
async fn main() {
    // example.com here can also be replaced with example.com:80
    let client: GreeterClient = GreeterClientBuilder::new("example.com").build();

    let req = volo_gen::proto_gen::helloworld::HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };

    let resp = client.say_hello(req).await;
    match resp {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("Request failed: {e:?}"),
    }
}
