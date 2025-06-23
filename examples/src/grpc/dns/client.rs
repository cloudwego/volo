// Ensure example.com is mapped to a local IP address (e.g. 127.0.0.1) before running this
// example by editing hosts file (e.g. /etc/hosts on Unix or
// C:\Windows\System32\drivers\etc\hosts on Windows)

use pilota::FastStr;
use volo_gen::proto_gen::helloworld::{GreeterClient, GreeterClientBuilder};
use volo_grpc::client::dns::DnsResolver;

#[volo::main]
async fn main() {
    let resolver = DnsResolver::default();

    // Perform DNS resolution for "example.com" on port 80
    let address = resolver
        .resolve("example.com", 80)
        .await
        .expect("DNS resolution failed");

    // Build a gRPC client for the Greeter service targeting the resolved socket address
    let client: GreeterClient = GreeterClientBuilder::new("hello").address(address).build();

    let req = volo_gen::proto_gen::helloworld::HelloRequest {
        name: FastStr::from_static_str("Volo"),
    };

    let resp = client.say_hello(req).await;
    match resp {
        Ok(info) => println!("{info:?}"),
        Err(e) => eprintln!("Request failed: {e:?}"),
    }
}
