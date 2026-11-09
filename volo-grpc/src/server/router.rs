use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use http_body::Body as HttpBody;
use motore::{BoxCloneService, Service};
use rustc_hash::FxHashMap;
use volo::Unwrap;

use super::{NamedService, policy::ResponsePolicy};
use crate::{
    Request, Response, Status,
    body::BoxBody,
    context::{Deadline, ServerContext},
    message::{PayloadKind, ServiceDescriptor},
    streaming::{CallGuard, HookFactory, StreamContext},
};

type RoutedService<B> = (
    BoxCloneService<ServerContext, Request<B>, Response<BoxBody>, Status>,
    Option<ServiceDescriptor>,
    &'static str,
    Option<HookFactory>,
);

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
    routes: FxHashMap<RouteId, RoutedService<B>>,
    node: matchit::Router<RouteId>,
    stream_hook: Option<HookFactory>,
}

impl<B> Clone for Router<B> {
    fn clone(&self) -> Self {
        Self {
            routes: self.routes.clone(),
            node: self.node.clone(),
            stream_hook: self.stream_hook.clone(),
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
            stream_hook: None,
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

        let hook = match (
            service.stream_hook_factory().map(HookFactory),
            self.stream_hook.clone(),
        ) {
            (Some(service), Some(default)) => Some(default.append(service)),
            (service, default) => service.or(default),
        };
        self.routes.insert(
            id,
            (
                BoxCloneService::new(service),
                S::descriptor(),
                S::NAME,
                hook,
            ),
        );

        self
    }

    pub(crate) fn with_stream_hook(mut self, next: HookFactory, front: bool) -> Self {
        for (_, _, _, hook) in self.routes.values_mut() {
            *hook = Some(match hook.take() {
                Some(previous) if front => next.clone().append(previous),
                Some(previous) => previous.append(next.clone()),
                None => next.clone(),
            });
        }
        self.stream_hook = Some(match self.stream_hook.take() {
            Some(previous) if front => next.append(previous),
            Some(previous) => previous.append(next),
            None => next,
        });
        self
    }

    pub(crate) fn response_payloads(&self) -> Option<Arc<FxHashMap<&'static str, PayloadKind>>> {
        if !self.routes.values().any(|(_, descriptor, _, _)| {
            descriptor.is_some_and(|d| d.payload == PayloadKind::Thrift)
        }) {
            return None;
        }
        Some(Arc::new(
            self.routes
                .values()
                .map(|(_, descriptor, name, _)| {
                    (*name, descriptor.map(|d| d.payload).unwrap_or_default())
                })
                .collect(),
        ))
    }

    pub(crate) fn requires_managed_transport(&self) -> bool {
        self.routes.values().any(|(_, descriptor, _, hook)| {
            hook.as_ref().is_some_and(|hook| match descriptor {
                Some(descriptor) => descriptor
                    .methods
                    .iter()
                    .any(|method| hook.accepts(Some(method)).unwrap_or(true)),
                None => hook.accepts(None).unwrap_or(true),
            }) || descriptor.is_some_and(|descriptor| descriptor.payload == PayloadKind::Thrift)
        })
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
        mut req: Request<B>,
    ) -> Result<Self::Response, Self::Error> {
        let path = cx.rpc_info.method();
        match self.node.at(path) {
            Ok(match_) => {
                let id = match_.value;
                let (route, descriptor, _, hook) = self.routes.get(id).volo_unwrap();
                let route = route.clone();
                let method = if let Some(descriptor) = descriptor {
                    let name = match_.params.get("rest").unwrap_or_default();
                    Some(
                        *descriptor
                            .methods
                            .iter()
                            .find(|method| method.name == name)
                            .ok_or_else(|| Status::unimplemented("unknown method"))?,
                    )
                } else {
                    None
                };
                let payload = descriptor.map(|d| d.payload).unwrap_or_default();
                if payload == PayloadKind::Thrift || cx.extensions.get::<ResponsePolicy>().is_some()
                {
                    cx.extensions.insert(ResponsePolicy {
                        payload,
                        http_status: None,
                    });
                }
                if let Some(method) = method {
                    req.extensions_mut().insert(method);
                    cx.extensions.insert(method);
                }
                let hook = match hook.as_ref() {
                    Some(hook) if hook.accepts(method.as_ref())? => Some(hook),
                    _ => None,
                };
                if req
                    .extensions()
                    .get::<Deadline>()
                    .copied()
                    .is_some_and(Deadline::is_expired)
                {
                    return Err(Status::deadline_exceeded("timeout"));
                }
                if descriptor.is_some_and(|descriptor| descriptor.payload == PayloadKind::Thrift)
                    || hook.is_some()
                {
                    // Allocate lifecycle state only for managed calls. This
                    // keeps ordinary Protobuf request futures small.
                    Box::pin(async move {
                        let stream = StreamContext::new(
                            cx.rpc_info.method().as_str(),
                            method.map(|method| method.mode),
                            volo::context::Role::Server,
                            req.extensions().get::<Deadline>().copied(),
                        );
                        if let Some(owner) = req.extensions().get::<super::h2::StreamOwner>() {
                            owner.attach(
                                stream.clone(),
                                descriptor.map(|value| value.payload).unwrap_or_default(),
                            );
                        }
                        if let Some(scope) = req
                            .extensions()
                            .get::<Arc<dyn crate::streaming::StreamScope>>()
                        {
                            stream.set_scope(scope.clone());
                        }
                        req.extensions_mut().insert(stream.clone());
                        cx.extensions.insert(stream.clone());
                        if let Some(method) = method {
                            stream.with_extensions(|extensions| extensions.insert(method));
                        }
                        let guard = CallGuard::new(stream.clone());
                        if let Err(status) = stream.sync_scope(|| stream.install_hook(hook)) {
                            stream.finish(status.clone());
                            return Err(status);
                        }
                        stream.arm_deadline();
                        let result = stream
                            .scope(async {
                                tokio::select! { biased;
                                    status = stream.cancelled() => Err(status),
                                    result = route.call(cx, req) => result,
                                }
                            })
                            .await;
                        match result {
                            Ok(mut response) => {
                                response.extensions_mut().insert(stream);
                                Ok(response.map(|body| {
                                    crate::streaming::lifecycle_body::response(body, guard)
                                }))
                            }
                            Err(status) => {
                                stream.finish(status.clone());
                                Err(status)
                            }
                        }
                    })
                    .await
                } else {
                    route.call(cx, req).await
                }
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
