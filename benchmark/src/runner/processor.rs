use crate::{benchmark::echo::Response, perf::Recoder, runner::Request};

pub const ECHO_ACTION: &str = "echo";
pub const BEGIN_ACTION: &str = "begin";
pub const END_ACTION: &str = "end";
pub const SLEEP_ACTION: &str = "sleep";
pub const REPORT_ACTION: &str = "report";

pub async fn process_request(recoder: &Recoder, req: Request) -> Response {
    match req.action.as_str() {
        BEGIN_ACTION => {
            recoder.begin().await;
        }
        END_ACTION => {
            recoder.end();
            recoder.report();
            return Response {
                action: REPORT_ACTION.into(),
                msg: recoder.report_string().into(),
            };
        }
        SLEEP_ACTION => {
            let time_str = req.msg.split(',').next().unwrap();
            if let Ok(n) = time_str.parse::<u64>() {
                tokio::time::sleep(tokio::time::Duration::from_millis(n)).await;
            }
        }
        _ => {}
    }
    Response {
        action: req.action,
        msg: req.msg,
    }
}

pub fn process_response(action: &str, msg: &str) {
    if action == REPORT_ACTION {
        println!("{}", msg);
    }
}
