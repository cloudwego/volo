use std::{
    cell::RefCell,
    hash::{Hash, Hasher},
    net::IpAddr,
    sync::LazyLock,
};

use metainfo::{MetaInfo, METAINFO};
use pilota::FastStr;
use volo::{
    discovery::StaticDiscover,
    loadbalance::{
        consistent_hash::{ConsistentHashBalance, ConsistentHashOption},
        RequestHash,
    },
};

static CLIENT: LazyLock<volo_gen::proto_gen::helloworld::GreeterClient> = LazyLock::new(|| {
    let discover = StaticDiscover::from(vec![
        "127.0.0.1:8080".parse().unwrap(),
        "127.0.0.2:8081".parse().unwrap(),
    ]);
    let lb = ConsistentHashBalance::new(ConsistentHashOption::default());
    volo_gen::proto_gen::helloworld::GreeterClientBuilder::new("hello")
        .load_balance(lb)
        .discover(discover)
        .build()
});

#[inline]
fn set_request_hash(code: u64) {
    metainfo::METAINFO
        .try_with(|m| m.borrow_mut().insert(RequestHash(code)))
        .unwrap();
}

fn get_local_ip() -> Option<IpAddr> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok()?.ip().into()
}

fn ip_to_u64(ip: &IpAddr) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    ip.hash(&mut hasher);
    hasher.finish()
}

#[volo::main]
async fn main() {
    let mi = MetaInfo::new();
    METAINFO
        .scope(RefCell::new(mi), async move {
            for _ in 0..3 {
                let req = volo_gen::proto_gen::helloworld::HelloRequest {
                    name: FastStr::from_static_str("Volo"),
                };
                set_request_hash(ip_to_u64(&get_local_ip().unwrap()));
                let resp = CLIENT.say_hello(req).await;
                match resp {
                    Ok(info) => println!("{info:?}"),
                    Err(e) => eprintln!("{e:?}"),
                }
            }
            for _ in 0..3 {
                let req = volo_gen::proto_gen::helloworld::HelloRequest {
                    name: FastStr::from_static_str("Volo"),
                };
                set_request_hash(1000);
                let resp = CLIENT.clone().say_hello(req).await;
                match resp {
                    Ok(info) => println!("{info:?}"),
                    Err(e) => eprintln!("{e:?}"),
                }
            }
        })
        .await;
}
