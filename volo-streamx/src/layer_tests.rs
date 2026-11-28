use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use volo::streaming::{
    Invocation, InvocationService, RpcError, RpcResult, ServerMiddleware, StreamContext,
};

use super::*;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}
struct Allocator;
fn allocated() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}
// SAFETY: Pointers and layouts are forwarded unchanged to System. Counting uses
// allocation-free thread locals and is enabled only around the measured calls.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

struct Pass;
struct PassService<S>(S);
impl<S> Layer<S> for Pass {
    type Service = PassService<S>;
    fn layer(self, inner: S) -> Self::Service {
        PassService(inner)
    }
}
impl<'a, S> Service<StreamContext, Invocation<'a>> for PassService<S>
where
    S: Service<StreamContext, Invocation<'a>, Response = (), Error = RpcError> + Send + Sync,
{
    type Response = ();
    type Error = RpcError;
    async fn call(&self, cx: &mut StreamContext, request: Invocation<'a>) -> RpcResult<()> {
        self.0.call(cx, request).await
    }
}

fn server() -> Server {
    static SERVICE: crate::ServiceDescriptor = crate::ServiceDescriptor {
        name: "S",
        wire_name: "S",
        payload: crate::PayloadKind::Thrift,
        methods: &[crate::MethodDescriptor {
            name: "M",
            mode: crate::RpcMode::Bidirectional,
            oneway: false,
        }],
    };
    let mut router = volo_thrift::transport::ttstream::router::StreamRouter::new();
    router.add("S", "M", |_, _| async { Ok(()) }).unwrap();
    Server::new().add_registration(crate::ServiceRegistration::new(&SERVICE).with_ttstream(router))
}

async fn allocations(middleware: Option<ServerMiddleware>) -> usize {
    let cx = ServerMiddleware::scope_optional(middleware, async {
        volo_grpc::streaming::StreamContext::new("/S/M", None, volo::context::Role::Server, None)
            .shared_context()
    })
    .await;
    // Initialize scope/task-local state outside the measured region.
    cx.invoke(|_| async { Ok(()) }).await.unwrap();
    ALLOCATIONS.with(|count| count.set(Some(0)));
    for _ in 0..100 {
        cx.invoke(|_| async { std::hint::black_box(Ok(())) })
            .await
            .unwrap();
    }
    ALLOCATIONS.with(|count| count.take().unwrap())
}

#[tokio::test]
async fn invocation_allocations_do_not_grow_with_layer_count() {
    let one = server().layer(Pass).prepare().unwrap();
    let four = server()
        .layer(Pass)
        .layer(Pass)
        .layer(Pass)
        .layer(Pass)
        .prepare()
        .unwrap();
    let one = allocations(one.state.middleware.clone()).await;
    let four = allocations(four.state.middleware.clone()).await;
    // Reconstruct the previous per-layer type erasure as a measured baseline.
    let mut legacy = InvocationService::default();
    for _ in 0..4 {
        legacy = InvocationService::new(Pass.layer(legacy));
    }
    let legacy = allocations(Some(ServerMiddleware::new(Some(legacy), Vec::new()))).await;
    println!(
        "100 invocations: one layer = {one}, four layers = {four}, previous four layers = \
         {legacy} allocations"
    );
    assert_eq!(
        one, four,
        "framework allocations must be independent of layer count"
    );
    assert_eq!(
        legacy,
        four + 400,
        "remove all four intermediate erased futures"
    );
}
