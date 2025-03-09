use rpc_provider::RpcProvider;
use volo_gen::proto_gen::google::cloud::beyondcorp::appgateways::v1;

mod header;
mod endpoint;
mod discover;
mod rpc_provider;

#[volo::main]
async fn main() {
    let provider = RpcProvider::new();
    
    let endpoint = "https://beyondcorp.googleapis.com:443/".parse().unwrap();
    let client = provider.app_gateway_service(endpoint);

    let req = volo_grpc::Request::new(v1::ListAppGatewaysRequest {
        parent: "".into(),
        page_size: 20,
        filter: "".into(),
        order_by: "".into(),
        page_token: "".into(),
    });

    let resp = client.list_app_gateways(req).await;
    println!("resp = {:#?}", resp);
}