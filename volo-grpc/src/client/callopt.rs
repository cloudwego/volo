//! This module provides the ability to set some options at call time.
//! These options also only apply to the call once.
//!
//! Note: If you set a [`CallOpt`] to a [`Client`][super::Client] and clones it,
//! the [`CallOpt`] will be discarded.
//!
//! # Example
//!
//! ```rust,ignore
//! use volo_grpc::client::CallOpt;
//!
//! lazy_static! {
//!     static ref CLIENT: volo_gen::volo::example::item::ItemServiceClient = {
//!         let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
//!         volo_gen::volo::example::item::ItemServiceClientBuilder::new("volo-example-item")
//!             .layer_inner(LogLayer)
//!             .address(addr)
//!             .build()
//!     };
//! }
//!
//! #[volo::main]
//! async fn main() {
//!     let callopt = CallOpt::default();
//!     // Do something with callopt here
//!     ...
//!     let req = volo_gen::volo::example::item::GetItemRequest { id: 1024 };
//!     let resp = CLIENT.clone().get_item(req).await;
//!     match resp {
//!         Ok(info) => tracing::info!("{:?}", info),
//!         Err(e) => tracing::error!("{:?}", e),
//!     }
//! }
//! ```

use metainfo::TypeMap;
use volo::net::Address;

use crate::context::Config;

#[derive(Debug, Default)]
pub struct CallOpt {
    /// Sets the callee tags for the call.
    pub callee_tags: TypeMap,
    /// Sets the address for the call.
    ///
    /// The client will skip the discovery and loadbalance Service if this is set.
    pub address: Option<Address>,
    pub config: Config,
    /// Sets the caller tags for the call.
    pub caller_tags: TypeMap,
}

impl CallOpt {
    /// Creates a new [`CallOpt`].
    pub fn new() -> Self {
        Default::default()
    }
}

impl volo::client::Apply<crate::context::ClientContext> for CallOpt {
    type Error = crate::Status;

    fn apply(self, cx: &mut crate::context::ClientContext) -> Result<(), Self::Error> {
        let caller = cx.rpc_info.caller_mut().unwrap();
        caller.tags.extend(self.caller_tags);

        let callee = cx.rpc_info.callee_mut().unwrap();
        callee.tags.extend(self.callee_tags);
        if let Some(addr) = self.address {
            callee.set_address(addr);
        }
        cx.rpc_info.config_mut().unwrap().merge(self.config);
        Ok(())
    }
}
