use std::collections::BTreeMap;

use pilota::thrift::{ApplicationException, ApplicationExceptionKind};
use sonic_rs::{JsonValueTrait, Value};
use ttstream_codegen::volo_gen::interop::Message;
use volo_thrift::streaming::{Metadata, RequestStream, ResponseSender, StreamError};

fn invalid(message: impl std::fmt::Display) -> StreamError {
    StreamError::Protocol(format!("errors fixture: {message}"))
}

pub async fn serve(
    mut input: RequestStream<Message>,
    mut output: ResponseSender<Message>,
) -> Result<(), StreamError> {
    let first = input
        .recv()
        .await?
        .ok_or_else(|| invalid("missing request"))?;
    let request: Value = sonic_rs::from_str(&first.value).map_err(invalid)?;
    if request.get("schemaVersion").as_u64() != Some(1)
        || request.get("id").as_str().is_none_or(str::is_empty)
        || first.seq != 1
    {
        return Err(invalid("invalid schemaVersion/id/seq"));
    }
    let kind = request
        .get("kind")
        .as_str()
        .ok_or_else(|| invalid("missing kind"))?
        .to_owned();
    if !matches!(kind.as_str(), "ok" | "biz" | "application") {
        return Err(invalid("invalid kind"));
    }
    let code = i32::try_from(
        request
            .get("code")
            .as_i64()
            .ok_or_else(|| invalid("missing code"))?,
    )
    .map_err(invalid)?;
    let message = request
        .get("message")
        .as_str()
        .ok_or_else(|| invalid("missing message"))?
        .to_owned();
    let extra = super::string_entries(request.get("extra"))
        .map_err(invalid)?
        .into_iter()
        .collect();
    let mut header = Metadata::default();
    header.insert("interop-error-header", "header-value");
    output.set_header(header)?;
    let mut trailer = Metadata::default();
    trailer.insert("interop-error-trailer", "trailer-value");
    output.set_trailer(trailer)?;
    output.send(&first).await?;
    match kind.as_str() {
        "biz" => Err(StreamError::Biz {
            code,
            message,
            extra,
        }),
        "application" => Err(StreamError::Application(ApplicationException::new(
            ApplicationExceptionKind::from_i32(code),
            message,
        ))),
        _ => Ok(()),
    }
}

pub fn metadata_strings(
    metadata: &Metadata,
) -> Result<BTreeMap<String, String>, std::str::Utf8Error> {
    metadata
        .string
        .iter()
        .map(|(key, value)| {
            Ok((
                std::str::from_utf8(key)?.to_owned(),
                std::str::from_utf8(value)?.to_owned(),
            ))
        })
        .collect()
}

pub fn observed(result: Result<Option<Message>, StreamError>) -> Result<Value, StreamError> {
    match result {
        Ok(Some(_)) => Err(invalid("unexpected extra DATA")),
        Ok(None) => Ok(
            sonic_rs::json!({"kind":"ok","code":0,"message":"","extra":{},"kitex_code":0,"error":null}),
        ),
        Err(error) => {
            let code = error.kitex_code();
            let description = error.to_string();
            Ok(match error {
                StreamError::Biz {
                    code: biz_code,
                    message,
                    extra,
                } => {
                    sonic_rs::json!({"kind":"biz","code":biz_code,"message":message,"extra":extra,"kitex_code":code,"error":description})
                }
                StreamError::Application(app) => {
                    sonic_rs::json!({"kind":"application","code":app.kind().as_i32(),"message":app.message(),"extra":{},"kitex_code":code,"error":description,"application_code_available":true})
                }
                _ => {
                    sonic_rs::json!({"kind":"unknown","code":null,"message":description,"extra":{},"kitex_code":code,"error":description})
                }
            })
        }
    }
}
