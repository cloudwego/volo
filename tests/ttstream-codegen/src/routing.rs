use sonic_rs::Value;
use ttstream_codegen::volo_gen::interop::Message;
use volo_thrift::{
    streaming::{OpenedStream, StreamContext, StreamError, StreamMode},
    transport::ttstream::router::{StreamMethod, StreamRouter},
};

async fn respond(
    cx: StreamContext,
    stream: OpenedStream,
    selected: &'static str,
) -> Result<(), StreamError> {
    let (mut input, mut output) = stream.into_server::<Message, Message>();
    let request = input
        .recv()
        .await?
        .ok_or_else(|| StreamError::Protocol("routing fixture missing request".into()))?;
    let body = sonic_rs::json!({"schemaVersion":1,"handler":selected,"service":cx.service(),"method":cx.method(),"request":request.value.as_str()});
    let value =
        sonic_rs::to_string(&body).map_err(|error| StreamError::Protocol(error.to_string()))?;
    output
        .send(&Message {
            value: value.into(),
            seq: request.seq,
        })
        .await
}
fn handler(selected: &'static str) -> StreamMethod {
    StreamMethod::new(StreamMode::Bidirectional, move |cx, stream| {
        respond(cx, stream, selected)
    })
}
fn register(
    router: &mut StreamRouter,
    name: &'static str,
    methods: &[&str],
    unary: &[&str],
    fallback: bool,
) -> Result<(), StreamError> {
    router.declare_service(name);
    for method in methods {
        router.add_mode(
            name,
            *method,
            StreamMode::Bidirectional,
            move |cx, stream| respond(cx, stream, name),
        )?;
    }
    for method in unary {
        router.add_unary_method(name, *method)?;
    }
    if fallback {
        router.fallback(name);
    }
    Ok(())
}
pub fn router(profile: &str) -> Result<StreamRouter, StreamError> {
    let mut router = StreamRouter::new();
    match profile {
        "unique" | "unknown" | "dynamic" | "binary-v2" => {
            register(&mut router, "A", &["OnlyA"], &["UnaryA"], false)?;
            match profile {
                "dynamic" => {
                    router.add_generic("B", |method| (method == "OnlyB").then(|| handler("B")))?
                }
                "binary-v2" => router.add_binary_generic("B", handler("B"))?,
                _ => register(&mut router, "B", &["OnlyB"], &[], false)?,
            }
            if profile == "unknown" {
                router.unknown_service(handler("unknown"))?;
            }
        }
        "fallback" | "strict" => {
            register(
                &mut router,
                "A",
                &["OnlyA", "Shared"],
                &[],
                profile == "fallback",
            )?;
            register(&mut router, "B", &["OnlyB", "Shared"], &[], false)?;
            router.refuse_traffic_without_service_name(profile == "strict");
        }
        "mixed" => {
            register(&mut router, "A", &["OnlyA"], &["Shared"], true)?;
            register(&mut router, "B", &["OnlyB", "Shared"], &[], false)?;
        }
        "combine" => {
            register(
                &mut router,
                "Combined",
                &["OnlyA", "OnlyB", "Shared"],
                &[],
                false,
            )?;
            router.combine_service("Combined")?;
        }
        _ => {
            return Err(StreamError::Configuration(format!(
                "unsupported routing profile {profile}"
            )));
        }
    }
    router.validate()?;
    Ok(router)
}
fn invalid_registration(mode: &str) -> (&'static str, Result<(), StreamError>) {
    let mut router = StreamRouter::new();
    let registration = (|| {
        match mode {
            "duplicate-service" => {
                register(&mut router, "A", &["OnlyA"], &[], false)?;
                let mut second = StreamRouter::new();
                register(&mut second, "A", &["OnlyB"], &[], false)?;
                router.merge(second);
            }
            "duplicate-fallback" => {
                register(&mut router, "A", &["OnlyA"], &[], true)?;
                register(&mut router, "B", &["OnlyB"], &[], true)?;
            }
            "duplicate-unknown" => {
                router.unknown_service(handler("unknown"))?;
                router.unknown_service(handler("unknown"))?;
            }
            "conflicting-methods" => {
                register(&mut router, "A", &["Shared"], &[], false)?;
                register(&mut router, "B", &["Shared"], &[], false)?;
            }
            "fallback-missing-conflict" => {
                register(&mut router, "A", &["OnlyA"], &[], true)?;
                register(&mut router, "B", &["Shared"], &[], false)?;
                register(&mut router, "C", &["Shared"], &[], false)?;
            }
            "combine-multi" | "combine-unknown" => {
                register(&mut router, "Combined", &["Shared"], &[], false)?;
                router.combine_service("Combined")?;
                if mode == "combine-multi" {
                    register(&mut router, "A", &["OnlyA"], &[], false)?;
                } else {
                    router.unknown_service(handler("unknown"))?;
                }
            }
            "binary-v1-multi" | "binary-v1-unknown" => {
                router.binary_generic_v1(handler("v1"))?;
                if mode == "binary-v1-multi" {
                    register(&mut router, "A", &["OnlyA"], &[], false)?;
                } else {
                    router.unknown_service(handler("unknown"))?;
                }
            }
            _ => {
                return Err(StreamError::Configuration(
                    "unknown registration profile".into(),
                ));
            }
        }
        Ok(())
    })();
    if registration.is_err() {
        ("register", registration)
    } else {
        ("validate", router.validate())
    }
}
pub fn registrations() -> Value {
    let mut ok = true;
    let mut cases = Vec::new();
    for name in [
        "duplicate-service",
        "duplicate-fallback",
        "duplicate-unknown",
        "conflicting-methods",
        "fallback-missing-conflict",
        "combine-multi",
        "combine-unknown",
        "binary-v1-multi",
        "binary-v1-unknown",
    ] {
        let (stage, result) = invalid_registration(name);
        ok &= result.is_err();
        cases.push(sonic_rs::json!({"name":name,"stage":stage,"outcome":if result.is_err(){"error"}else{"unexpected-success"},"error":result.err().map(|e|e.to_string())}));
    }
    sonic_rs::json!({"event":"result","api":"routing-registration","profile":"rust","ok":ok,"cases":cases})
}
