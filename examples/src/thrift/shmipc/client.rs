use std::sync::LazyLock;

use motore::{layer::Layer, service::Service};
use volo::{context::Context, net::Address};
use volo_thrift::{
    ClientError,
    client::CallOpt,
    context::ClientContext,
    transport::{DialPlan, SelectedTransport},
};

/// A per-request layer that dials shmipc first and falls back to a UDS/TCP address.
///
/// It follows the `callee.address()` contract: it sets the primary (shmipc) address on the callee
/// *before* injecting the [`DialPlan`], so `volo-thrift` can validate that the plan primary matches
/// the callee address. The pool key is chosen per-attempt from the actually selected address, so a
/// UDS/TCP fallback transport never pollutes the shmipc key. There is no `.dial_plan(...)` builder;
/// the plan is always injected through the request context.
#[derive(Clone)]
struct ShmipcFallbackLayer {
    plan: DialPlan,
}

impl ShmipcFallbackLayer {
    fn new(shmipc_addr: Address, fallback_addr: Address) -> Self {
        Self {
            plan: DialPlan::with_fallback(shmipc_addr, fallback_addr),
        }
    }
}

impl<S> Layer<S> for ShmipcFallbackLayer {
    type Service = ShmipcFallbackService<S>;

    fn layer(self, inner: S) -> Self::Service {
        ShmipcFallbackService {
            inner,
            plan: self.plan,
        }
    }
}

#[derive(Clone)]
struct ShmipcFallbackService<S> {
    inner: S,
    plan: DialPlan,
}

impl<S, Req> Service<ClientContext, Req> for ShmipcFallbackService<S>
where
    S: Service<ClientContext, Req, Error = ClientError> + Send + Sync + 'static,
    Req: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(&self, cx: &mut ClientContext, req: Req) -> Result<Self::Response, Self::Error> {
        // 1. Set the primary address first, then inject the plan (order matters).
        cx.rpc_info_mut()
            .callee_mut()
            .set_address(self.plan.primary().clone());
        cx.extensions_mut().insert(self.plan.clone());

        let resp = self.inner.call(cx, req).await;

        // The actually selected transport is written back by volo-thrift.
        if let Some(selected) = cx.extensions().get::<SelectedTransport>() {
            println!(
                "selected transport: {} (attempt {})",
                selected.address(),
                selected.attempt()
            );
        }
        resp
    }
}

static CLIENT: LazyLock<volo_gen::thrift_gen::hello::HelloServiceClient> = LazyLock::new(|| {
    let shmipc_path =
        std::os::unix::net::SocketAddr::from_pathname("/tmp/hello_test.sock").unwrap();
    let shmipc_addr = Address::from(volo::net::ShmipcAddr(shmipc_path));
    // Fallback to a plain UDS address when shmipc is unavailable.
    let fallback_path =
        std::os::unix::net::SocketAddr::from_pathname("/tmp/hello_fallback.sock").unwrap();
    let fallback_addr = Address::from(fallback_path);

    volo_gen::thrift_gen::hello::HelloServiceClientBuilder::new("hello")
        .address(shmipc_addr.clone())
        .layer_outer_front(ShmipcFallbackLayer::new(shmipc_addr, fallback_addr))
        .build()
});

#[volo::main]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let config = volo::net::shmipc::config::Config {
        share_memory_path_prefix: "/dev/shm/client.ipc.shm".to_string(),
        mem_map_type: volo::net::shmipc::config::MemMapType::MemMapTypeMemFd,
        ..Default::default()
    };
    volo::net::shmipc::config::DEFAULT_SHMIPC_CONFIG.store(config.into());

    let desc = volo_gen::thrift_gen::hello::HelloRequest::get_descriptor()
        .unwrap()
        .type_descriptor();
    println!("{desc:?}");

    loop {
        let fm = pilota_thrift_fieldmask::FieldMaskBuilder::new(&desc, &["$.hello"])
            .with_options(pilota_thrift_fieldmask::Options::new().with_black_list_mode(true))
            .build()
            .unwrap();
        println!("{fm:?}");
        let mut req = volo_gen::thrift_gen::hello::HelloRequest {
            name: "volo".into(),
            hello: Some("world".into()),
            _field_mask: None,
        };
        req.set_field_mask(fm);

        println!("req with field mask: {req:?}");
        let resp = CLIENT
            .clone()
            .with_callopt(CallOpt::default())
            .hello(req)
            .await;
        match resp {
            Ok(info) => println!("{info:?}"),
            Err(e) => eprintln!("{e:?}"),
        }
        // tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}
