#![cfg(feature = "ttstream")]
use std::sync::{Arc, Mutex};

use ttstream_codegen::volo_gen::interop::InteropClientBuilder;
use volo::{net::Address, service::Service};
use volo_thrift::streaming::{OpenStream, OpenedStream, StreamContext, StreamError};

struct Zone;
type Observation = (Option<Address>, Option<Address>, Option<String>);
struct Capture(Arc<Mutex<Option<Observation>>>);
impl Service<StreamContext, OpenStream> for Capture {
    type Response = OpenedStream;
    type Error = StreamError;
    async fn call(
        &self,
        cx: &mut StreamContext,
        request: OpenStream,
    ) -> Result<OpenedStream, StreamError> {
        let (address, tag) = cx.with_rpc_info(|rpc| {
            (
                rpc.callee().address(),
                rpc.callee()
                    .faststr_tags
                    .get::<Zone>()
                    .map(ToString::to_string),
            )
        });
        *self.0.lock().unwrap() = Some((request.address, address, tag));
        Err(StreamError::configuration("observation complete"))
    }
}
#[tokio::test]
async fn generated_streaming_callopt_applies_address_and_tags_once() {
    let original: Address = "127.0.0.1:1"
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .into();
    let target: Address = "127.0.0.1:2"
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .into();
    let captured = Arc::new(Mutex::new(None));
    let client = InteropClientBuilder::new("Interop")
        .address(original.clone())
        .streaming_service(Capture(captured.clone()))
        .build();
    let mut option = volo_thrift::client::CallOpt {
        address: Some(target.clone()),
        ..Default::default()
    };
    option.callee_faststr_tags.insert::<Zone>("once".into());
    let _ = client.clone().with_callopt(option).bidi().await;
    let actual = captured.lock().unwrap().take().unwrap();
    assert_eq!(
        actual,
        (Some(target.clone()), Some(target), Some("once".into()))
    );
    let _ = client.bidi().await;
    assert_eq!(
        captured.lock().unwrap().take().unwrap(),
        (Some(original.clone()), Some(original), None)
    );
}

mod timeout_tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use ttstream_codegen::volo_gen::interop::InteropClientBuilder;
    use volo::service::Service;
    use volo_thrift::streaming::{OpenStream, OpenedStream, StreamContext, StreamError};
    type Observation = Arc<Mutex<Option<(Option<Duration>, Option<Duration>)>>>;
    struct Pending(Observation);
    impl Service<StreamContext, OpenStream> for Pending {
        type Response = OpenedStream;
        type Error = StreamError;
        async fn call(
            &self,
            cx: &mut StreamContext,
            _: OpenStream,
        ) -> Result<OpenedStream, StreamError> {
            *self.0.lock().unwrap() = Some((
                cx.with_rpc_info(|rpc| rpc.config().rpc_timeout()),
                cx.deadline()
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
            ));
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn explicit_per_call_timeout_overrides_builder_timeout() {
        let observation = Arc::new(Mutex::new(None));
        let client = InteropClientBuilder::new("Interop")
            .rpc_timeout(Some(Duration::from_secs(1)))
            .streaming_service(Pending(observation.clone()))
            .build();
        let mut option = volo_thrift::client::CallOpt::default();
        option
            .config
            .set_rpc_timeout(Some(Duration::from_millis(5)));
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            client.with_callopt(option).bidi(),
        )
        .await;
        let observed = *observation.lock().unwrap();
        eprintln!(
            "configured/per-stream remaining: {observed:?}; outer timeout: {}",
            result.is_err()
        );
        assert!(
            result.is_ok(),
            "explicit 5ms call timeout did not finish before the independent 50ms observation \
             bound"
        );
        assert_eq!(
            result.unwrap().err().and_then(|e| e.kitex_code()),
            Some(12015)
        );
    }
    #[tokio::test]
    async fn default_callopt_preserves_stream_timeout_without_unary_implicit_default() {
        for deadline in [None, Some(Duration::from_secs(30))] {
            let observation = Arc::new(Mutex::new(None));
            let client = InteropClientBuilder::new("Interop")
                .streaming_config(volo_thrift::streaming::StreamConfig {
                    deadline,
                    ..Default::default()
                })
                .streaming_service(Pending(observation.clone()))
                .build();
            let result = tokio::time::timeout(
                Duration::from_millis(1),
                client
                    .with_callopt(volo_thrift::client::CallOpt::default())
                    .bidi(),
            )
            .await;
            assert!(result.is_err());
            let (_, observed) = observation.lock().unwrap().unwrap();
            assert_eq!(observed.is_some(), deadline.is_some());
            if let Some(observed) = observed {
                assert!(observed > Duration::from_secs(29));
            }
        }
    }
    #[tokio::test]
    async fn explicit_builder_timeout_control_is_effective() {
        let observation = Arc::new(Mutex::new(None));
        let client = InteropClientBuilder::new("Interop")
            .rpc_timeout(Some(Duration::from_millis(5)))
            .streaming_service(Pending(observation))
            .build();
        let result = tokio::time::timeout(Duration::from_millis(50), client.bidi()).await;
        assert_eq!(
            result.unwrap().err().and_then(|e| e.kitex_code()),
            Some(12015)
        );
    }
}

mod connect_timeout {
    use std::{
        io,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use ttstream_codegen::volo_gen::interop::InteropClientBuilder;
    use volo::net::{Address, dial::MakeTransport};

    #[derive(Clone)]
    struct CaptureDial {
        configured: Option<Duration>,
        observed: Arc<Mutex<Vec<Option<Duration>>>>,
    }
    impl MakeTransport for CaptureDial {
        type ReadHalf = volo::net::conn::OwnedReadHalf;
        type WriteHalf = volo::net::conn::OwnedWriteHalf;
        async fn make_transport(
            &self,
            _: Address,
        ) -> io::Result<(Self::ReadHalf, Self::WriteHalf)> {
            self.observed.lock().unwrap().push(self.configured);
            Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "intentional observation only",
            ))
        }
        fn set_connect_timeout(&mut self, timeout: Option<Duration>) {
            self.configured = timeout;
        }
        fn set_read_timeout(&mut self, _: Option<Duration>) {}
        fn set_write_timeout(&mut self, _: Option<Duration>) {}
    }

    #[tokio::test]
    async fn explicit_per_call_connect_timeout_reaches_transport_without_leaking() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let client = InteropClientBuilder::new("Interop")
            .address(Address::Ip("127.0.0.1:1".parse().unwrap()))
            .make_transport(CaptureDial {
                configured: None,
                observed: observed.clone(),
            })
            .connect_timeout(Some(Duration::from_secs(1)))
            .build();
        let mut option = volo_thrift::client::CallOpt::default();
        option
            .config
            .set_connect_timeout(Some(Duration::from_millis(5)));
        assert!(client.clone().with_callopt(option).bidi().await.is_err());
        assert!(client.bidi().await.is_err());
        let actual = observed.lock().unwrap().clone();
        eprintln!("actual dial timeout values: {actual:?}");
        assert_eq!(
            actual,
            vec![Some(Duration::from_millis(5)), Some(Duration::from_secs(1))]
        );
    }
}

#[test]
fn generated_streaming_builder_enables_transport_without_user_protocol_call() {
    assert!(InteropClientBuilder::new("Interop").streaming_enabled());
    assert!(
        ttstream_codegen::volo_gen::audit::ChildClientBuilder::new("Child").streaming_enabled()
    );
}
