use std::{
    fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use http_body::Body as HttpBody;
use motore::{BoxCloneService, Service};
use rustc_hash::FxHashMap;
use volo::Unwrap;

use super::NamedService;
use crate::{body::BoxBody, context::ServerContext, Request, Response, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RouteId(u32);

impl RouteId {
    fn next() -> Self {
        // `AtomicU64` isn't supported on all platforms
        static ID: AtomicU32 = AtomicU32::new(0);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        if id == u32::MAX {
            panic!("Over `u32::MAX` routes created. If you need this, please file an issue.");
        }
        Self(id)
    }
}

#[derive(Default)]
pub struct Router<B = BoxBody> {
    routes:
        FxHashMap<RouteId, BoxCloneService<ServerContext, Request<B>, Response<BoxBody>, Status>>,
    node: matchit::Router<RouteId>,
}

impl<B> Clone for Router<B> {
    fn clone(&self) -> Self {
        Self {
            routes: self.routes.clone(),
            node: self.node.clone(),
        }
    }
}

impl<B> Router<B>
where
    B: HttpBody + 'static,
{
    pub fn new() -> Self {
        Self {
            routes: Default::default(),
            node: Default::default(),
        }
    }

    pub fn add_service<S>(mut self, service: S) -> Self
    where
        S: Service<ServerContext, Request<B>, Response = Response<BoxBody>, Error = Status>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let path = format!("/{}/{{*rest}}", S::NAME);

        if path.is_empty() {
            panic!("[VOLO] Paths must start with a `/`. Use \"/\" for root routes");
        } else if !path.starts_with('/') {
            panic!("[VOLO] Paths must start with a `/`");
        }

        let id = RouteId::next();

        self.set_node(path, id);

        self.routes.insert(id, BoxCloneService::new(service));

        self
    }

    #[track_caller]
    fn set_node(&mut self, path: String, id: RouteId) {
        if let Err(err) = self.node.insert(path, id) {
            panic!("[VOLO] Invalid route: {err}");
        }
    }
}

impl<B> Service<ServerContext, Request<B>> for Router<B>
where
    B: HttpBody + Send,
{
    type Response = Response<BoxBody>;
    type Error = Status;

    async fn call(
        &self,
        cx: &mut ServerContext,
        req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        let path = cx.rpc_info.method();
        match self.node.at(path) {
            Ok(match_) => {
                let id = match_.value;
                let route = self.routes.get(id).volo_unwrap().clone();
                route.call(cx, req).await
            }
            Err(err) => Err(Status::unimplemented(err.to_string())),
        }
    }
}

impl<B> fmt::Debug for Router<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router")
            .field("routes", &self.routes)
            .finish()
    }
}
