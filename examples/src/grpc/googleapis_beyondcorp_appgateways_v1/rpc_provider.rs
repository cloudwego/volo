use std::sync::Arc;

use volo::net::tls::{ClientTlsConfig, TlsConnector, TlsConnectorBuilder};
use volo_gen::proto_gen::google::cloud::beyondcorp::appgateways::v1::{
    AppGatewaysServiceClient, AppGatewaysServiceClientBuilder,
};
use volo_grpc::metadata::MetadataMap;
use volo_http::client::dns::DnsResolver;

use crate::{discover::LazyDiscover, endpoint::RpcEndpoint, header::HeaderLayer};

macro_rules! service_impl {
    ($builder_ty:ty, $self:expr, $endpoint:expr) => {{
        let mut builder = <$builder_ty>::new($self.callee_name.to_string())
            .discover(LazyDiscover::new($endpoint.clone()))
            .caller_name($self.caller_name.to_string())
            .layer_inner_front(
                HeaderLayer::new($endpoint.clone()).metadata($self.metadata.clone()),
            );
        if $endpoint.tls {
            let sni = $endpoint.server_name.unwrap_or_default();
            builder = builder.tls_config($self.tls_connector(sni));
        }
        builder.build()
    }};
}

struct RpcProviderInner {
    tls_connector: TlsConnector,
}

impl RpcProviderInner {
    fn new(tls_connector: TlsConnector) -> Self {
        Self { tls_connector }
    }
}

#[derive(Clone)]
pub struct RpcProvider {
    caller_name: String,
    callee_name: String,

    metadata: Option<MetadataMap>,
    inner: Arc<RpcProviderInner>,
}

impl RpcProvider {
    pub fn new() -> Self {
        let tls_connector = TlsConnectorBuilder::default()
            .add_alpn_protocol("h2")
            .build()
            .unwrap();
        Self {
            caller_name: "volo".into(),
            callee_name: "app_gateway_service".into(),

            metadata: None,
            inner: Arc::new(RpcProviderInner::new(tls_connector)),
        }
    }

    fn tls_connector(&self, server_name: String) -> ClientTlsConfig {
        ClientTlsConfig::new(server_name, self.inner.tls_connector.clone())
    }

    pub fn app_gateway_service(&self, endpoint: RpcEndpoint) -> AppGatewaysServiceClient {
        service_impl!(AppGatewaysServiceClientBuilder, self, endpoint)
    }
}
