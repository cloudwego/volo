use std::net::SocketAddr;

use benchmark::{
    benchmark::echo::{EchoServerClientBuilder, Request},
    perf::Recoder,
    runner::{
        Runner,
        processor::{BEGIN_ACTION, ECHO_ACTION, END_ACTION, SLEEP_ACTION, process_response},
    },
};
use clap::Parser;
use volo_thrift::codec::DefaultMakeCodec;

#[derive(Parser, Debug)] // requires `derive` feature
#[command(term_width = 0)] // Just to make testing across clap features easier
struct Args {
    #[arg(short = 'a', long, default_value = "127.0.0.1:8001")]
    /// client call address
    address: String,

    /// echo size
    #[arg(short = 'b', long, default_value_t = 1024)]
    echo_size: usize,

    /// call concurrent
    #[arg(short = 'c', long, default_value_t = 100)]
    concurrent: usize,

    /// call qps
    #[arg(short = 'q', long, default_value_t = 0)]
    qps: usize,

    /// call total nums
    #[arg(short = 'n', long, default_value_t = 1024 * 100)]
    total: usize,

    /// sleep time for every request handler
    #[arg(short = 's', long, default_value_t = 0)]
    sleep_time: usize,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let addr = args.address.parse::<SocketAddr>().unwrap();
    let client = EchoServerClientBuilder::new("test.echo.volo")
        .make_codec(DefaultMakeCodec::framed())
        .address(addr)
        .build();
    let mut payload = unsafe { String::from_utf8_unchecked(vec![0u8; args.echo_size]) };
    let mut action = ECHO_ACTION.into();
    if args.sleep_time > 0 {
        action = SLEEP_ACTION.into();
        let st = args.sleep_time.to_string();
        payload = format!("{},{}", st, &payload[st.len() + 1..]);
    }
    let req = Request {
        action,
        msg: payload.into(),
    };
    let r = Runner::new(args.qps);
    r.warmup(client.clone(), req.clone(), args.concurrent, 100 * 1000)
        .await;

    let _ = client
        .echo(Request {
            action: BEGIN_ACTION.into(),
            msg: "empty".into(),
        })
        .await
        .expect("beginning server failed");

    let recoder = Recoder::new("VOLO@Client");
    recoder.begin().await;
    r.run(
        client.clone(),
        req,
        args.concurrent,
        args.total,
        args.qps,
        args.sleep_time,
        args.echo_size,
        "volo",
    )
    .await;
    recoder.end();

    let resp = client
        .echo(Request {
            action: END_ACTION.into(),
            msg: "empty".into(),
        })
        .await
        .expect("ending server failed");
    process_response(&resp.action, &resp.msg);

    recoder.report();
}
