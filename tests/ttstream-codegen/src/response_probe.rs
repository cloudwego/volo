use std::{
    future::Future,
    time::{Duration, Instant},
};

use sonic_rs::Value;
use ttstream_codegen::volo_gen::interop::Message;
use volo_thrift::streaming::{BidiStreaming, StreamError};

// This is an observer: malformed peer responses are results, not fixture failures.
pub async fn observe(
    open: impl Future<Output = Result<BidiStreaming<Message, Message>, StreamError>>,
) -> Value {
    let started = Instant::now();
    let mut open_elapsed = None;
    let mut responses = Vec::new();
    let mut terminal = None;
    let mut header = None;
    let mut stage = "open";
    let operation = async {
        let stream = open.await?;
        open_elapsed = Some(started.elapsed().as_secs_f64());
        let (mut tx, mut rx) = stream.into_split();
        stage = "send";
        tx.send(&Message {
            value: "response-probe".into(),
            seq: 1,
        })
        .await?;
        stage = "half-close";
        tx.close_send().await?;
        stage = "recv";
        for _ in 0..8 {
            match rx.recv().await {
                Ok(Some(message)) => responses
                    .push(sonic_rs::json!({"value":message.value.as_str(),"seq":message.seq})),
                result => {
                    terminal = Some(super::error_status::observed(result)?);
                    break;
                }
            }
        }
        if terminal.is_none() {
            terminal = Some(
                sonic_rs::json!({"kind":"observation-limit","code":null,"kitex_code":null,"error":"eight DATA frames received without terminal"}),
            );
        }
        // Some malformed peers never send HEADER; inspection must not mask terminal.
        if let Ok(Ok(metadata)) = tokio::time::timeout(Duration::from_millis(50), rx.header()).await
        {
            header = super::error_status::metadata_strings(&metadata).ok();
        }
        Ok::<(), StreamError>(())
    };
    match tokio::time::timeout(Duration::from_secs(5), operation).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => terminal = super::error_status::observed(Err(error)).ok(),
        Err(_) if terminal.is_none() => {
            terminal = Some(
                sonic_rs::json!({"kind":"observation-timeout","code":null,"kitex_code":null,"error":"five second observation limit reached"}),
            )
        }
        Err(_) => {}
    }
    sonic_rs::json!({"event":"result","api":"response-probe","ok":true,"responses":responses,"terminal":terminal,"header":header,"open_elapsed_seconds":open_elapsed,"elapsed_seconds":started.elapsed().as_secs_f64(),"stage":stage})
}
