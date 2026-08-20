//! Pool-aware transport acquisition with in-order address fallback.
//!
//! [`DialPlan`] describes an ordered list of candidate [`Address`]es to try for a single request.
//! It is injected per-request through [`ClientContext`] extensions. The crate-private
//! `TransportAcquirer` performs the candidate walk *before* the request is sent, so every attempt
//! uses the real address as its pool key and no request object is ever cloned.
//!
//! The candidate walk lives only here; both the pingpong and multiplex clients delegate to
//! `TransportAcquirer::acquire` and keep only their protocol-specific send / EOF / reuse logic.

use std::{fmt::Write as _, io, sync::Arc};

use motore::service::UnaryService;
use volo::{context::Context, net::Address};

use crate::{
    ClientError,
    context::ClientContext,
    transport::pool::{Config, Key, Poolable, Pooled, PooledMakeTransport, Ver},
};

/// An ordered plan of transport addresses to try for a single request.
///
/// The first address is the primary; any following addresses are fallbacks that are only attempted
/// when acquiring the previous candidate fails. Construct it through [`DialPlan::new`] or
/// [`DialPlan::with_fallback`] and inject it per-request via `ClientContext::extensions_mut`.
///
/// The internal storage is an [`Arc`]-backed slice, so cloning a `DialPlan` only bumps a reference
/// count and never allocates or clones the request.
#[derive(Clone, Debug)]
pub struct DialPlan {
    // Invariant: always contains at least one address, with no consecutive duplicates.
    attempts: Arc<[Address]>,
}

impl DialPlan {
    /// Creates a plan that only ever dials `primary`.
    pub fn new(primary: Address) -> Self {
        Self {
            attempts: Arc::from(vec![primary]),
        }
    }

    /// Creates a plan that dials `primary` first and falls back to `fallback`.
    ///
    /// If `fallback` equals `primary`, the duplicate is dropped and the plan degrades to a single
    /// address.
    pub fn with_fallback(primary: Address, fallback: Address) -> Self {
        let attempts = if primary == fallback {
            vec![primary]
        } else {
            vec![primary, fallback]
        };
        Self {
            attempts: Arc::from(attempts),
        }
    }

    /// Returns the primary (first) address of the plan.
    pub fn primary(&self) -> &Address {
        // The constructors guarantee at least one address.
        &self.attempts[0]
    }

    /// Returns an iterator over all candidate addresses in dial order.
    pub fn attempts(&self) -> impl ExactSizeIterator<Item = &Address> {
        self.attempts.iter()
    }
}

/// The transport actually selected by the transport acquirer.
///
/// It is written into the request's `ClientContext` extensions so downstream layers (logging,
/// metrics) can report the real transport instead of the configured one.
#[derive(Clone, Debug)]
pub struct SelectedTransport {
    address: Address,
    attempt: usize,
}

impl SelectedTransport {
    /// Constructs a `SelectedTransport`.
    ///
    /// This is only intended for the acquire layer and for downstream tests that need to simulate a
    /// selected transport; regular code should read it back from the context extensions.
    #[doc(hidden)]
    pub fn new(address: Address, attempt: usize) -> Self {
        Self { address, attempt }
    }

    /// The address that was actually connected.
    pub fn address(&self) -> &Address {
        &self.address
    }

    /// The zero-based index of the successful attempt in the [`DialPlan`].
    pub fn attempt(&self) -> usize {
        self.attempt
    }
}

/// A leased transport, either owned directly (shmipc, which manages its own stream pool) or checked
/// out from the outer connection [`Pool`](crate::transport::pool::Pool).
pub(crate) enum TransportLease<K: Key, T: Poolable> {
    /// A transport that bypasses the outer pool. Used for shmipc, whose `SessionManager` already
    /// pools streams; the caller is responsible for stream reuse via the shmipc helper.
    #[cfg(feature = "shmipc")]
    Direct(T),
    /// A transport checked out from the outer pool, keyed by its real address.
    Pooled(Pooled<K, T>),
}

impl<K: Key, T: Poolable> TransportLease<K, T> {
    /// Shared access for protocols whose `send` takes `&self` (multiplex), and for the pingpong
    /// shmipc helper reuse path.
    #[cfg(any(feature = "shmipc", feature = "multiplex"))]
    pub(crate) fn transport(&self) -> &T {
        match self {
            #[cfg(feature = "shmipc")]
            TransportLease::Direct(t) => t,
            TransportLease::Pooled(p) => p.as_ref(),
        }
    }

    /// Exclusive access for protocols whose `send` takes `&mut self` (pingpong).
    pub(crate) fn transport_mut(&mut self) -> &mut T {
        match self {
            #[cfg(feature = "shmipc")]
            TransportLease::Direct(t) => t,
            TransportLease::Pooled(p) => p.as_mut(),
        }
    }

    /// Consumes the lease, returning a pooled transport to its pool.
    ///
    /// A [`TransportLease::Direct`] transport is simply dropped here: shmipc stream reuse is driven
    /// by the caller through the shmipc helper before the lease is dropped.
    pub(crate) async fn reuse(self) {
        match self {
            #[cfg(feature = "shmipc")]
            TransportLease::Direct(_t) => {}
            TransportLease::Pooled(p) => p.reuse().await,
        }
    }
}

/// The result of a successful [`TransportAcquirer::acquire`]: a leased transport ready to `send`.
pub(crate) struct AcquiredTransport<K: Key, T: Poolable> {
    lease: TransportLease<K, T>,
}

impl<K: Key, T: Poolable> AcquiredTransport<K, T> {
    /// Shared access to the underlying transport.
    #[cfg(any(feature = "shmipc", feature = "multiplex"))]
    pub(crate) fn transport(&self) -> &T {
        self.lease.transport()
    }

    /// Exclusive access to the underlying transport.
    pub(crate) fn transport_mut(&mut self) -> &mut T {
        self.lease.transport_mut()
    }

    /// Consumes the acquired transport, returning any pooled connection to its pool.
    pub(crate) async fn reuse(self) {
        self.lease.reuse().await
    }
}

/// Resolved candidate source for a single request: either the per-request [`DialPlan`] or a single
/// address fast path derived from `callee.address()`.
enum ResolvedPlan {
    /// No `DialPlan` in the context: dial the single callee address.
    Single(Address),
    /// A per-request `DialPlan` from the context extensions.
    Plan(DialPlan),
}

impl ResolvedPlan {
    fn len(&self) -> usize {
        match self {
            ResolvedPlan::Single(_) => 1,
            ResolvedPlan::Plan(plan) => plan.attempts.len(),
        }
    }

    fn candidate(&self, idx: usize) -> &Address {
        match self {
            ResolvedPlan::Single(addr) => addr,
            ResolvedPlan::Plan(plan) => &plan.attempts[idx],
        }
    }
}

/// Resolves the candidate source for this request.
///
/// The `callee.address()` contract is enforced by the caller before this is invoked; here we only
/// validate that a context `DialPlan` (if any) agrees with the already-read `target`.
fn resolve_dial_plan(cx: &ClientContext, target: &Address) -> Result<ResolvedPlan, ClientError> {
    match cx.extensions().get::<DialPlan>() {
        Some(plan) => {
            if plan.primary() != target {
                return Err(ClientError::Transport(
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "DialPlan primary ({}) does not match callee address ({target})",
                            plan.primary(),
                        ),
                    )
                    .into(),
                ));
            }
            // Cloning only bumps the Arc refcount; ends the borrow of `cx` immediately.
            Ok(ResolvedPlan::Plan(plan.clone()))
        }
        None => Ok(ResolvedPlan::Single(target.clone())),
    }
}

fn missing_address_error(cx: &ClientContext) -> ClientError {
    ClientError::Transport(
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("address is required, rpc_info: {:?}", cx.rpc_info()),
        )
        .into(),
    )
}

/// Acquires a transport for a single request, walking the [`DialPlan`] candidates in order.
///
/// The same generic type is monomorphized separately for the pingpong and multiplex makers; the two
/// clients never share a maker instance, only this acquire algorithm.
pub(crate) struct TransportAcquirer<MT>
where
    MT: UnaryService<Address>,
    MT::Response: Poolable,
{
    // Direct maker used to bypass the outer pool for shmipc candidates.
    #[cfg(feature = "shmipc")]
    direct: MT,
    // Pool-aware maker used for UDS/TCP candidates, keyed by the real address.
    pooled: PooledMakeTransport<MT, Address>,
}

impl<MT> Clone for TransportAcquirer<MT>
where
    MT: UnaryService<Address> + Clone,
    MT::Response: Poolable,
{
    fn clone(&self) -> Self {
        Self {
            #[cfg(feature = "shmipc")]
            direct: self.direct.clone(),
            pooled: self.pooled.clone(),
        }
    }
}

impl<MT> TransportAcquirer<MT>
where
    MT: UnaryService<Address> + Send + Sync + Clone + 'static,
    MT::Response: Poolable + Send + 'static,
    MT::Error: Into<ClientError> + Send,
{
    /// Builds an acquirer from a maker and an optional pool config.
    pub(crate) fn new(direct: MT, pool_cfg: Option<Config>) -> Self {
        #[cfg(feature = "shmipc")]
        {
            Self {
                pooled: PooledMakeTransport::new(direct.clone(), pool_cfg),
                direct,
            }
        }
        #[cfg(not(feature = "shmipc"))]
        {
            Self {
                pooled: PooledMakeTransport::new(direct, pool_cfg),
            }
        }
    }

    /// Acquires a transport, trying each [`DialPlan`] candidate until one succeeds.
    ///
    /// This records the make-transport start/end stats, rewrites `callee.address` to the actually
    /// attempted address, and writes a [`SelectedTransport`] into the context extensions. It never
    /// touches the request object.
    pub(crate) async fn acquire(
        &self,
        cx: &mut ClientContext,
        ver: Ver,
    ) -> Result<AcquiredTransport<Address, MT::Response>, ClientError> {
        // A retry layer may reuse this context across attempts. Clear any SelectedTransport left by
        // a previous acquire so that if this acquire fails entirely, downstream layers (metrics,
        // logging) do not report the previous success's transport. It is re-inserted below only
        // when a candidate succeeds.
        cx.extensions_mut().remove::<SelectedTransport>();

        // `callee.address()` is mandatory and represents the primary transport target.
        let target = cx
            .rpc_info()
            .callee()
            .address()
            .ok_or_else(|| missing_address_error(cx))?;

        // Resolve candidates and validate any context plan; this ends the borrow of `cx`.
        let resolved = resolve_dial_plan(cx, &target)?;

        cx.stats.record_make_transport_start_at();

        let n = resolved.len();
        let mut last_err: Option<ClientError> = None;
        let mut summary = String::new();

        for attempt in 0..n {
            let candidate = resolved.candidate(attempt).clone();
            // Point the callee at the address we are about to dial, so metrics/errors reflect the
            // real attempt. The borrow ends before the `await` below.
            cx.rpc_info_mut()
                .callee_mut()
                .set_address(candidate.clone());

            // Only time fallback attempts. The healthy `attempt == 0` success path (including a
            // shmipc primary that succeeds while a fallback is configured) reads no clock at all; a
            // first-attempt failure derives its latency from `make_transport_start_at` in the cold
            // log branch below.
            let fallback_start = (attempt > 0).then(std::time::Instant::now);

            match self.acquire_candidate(candidate.clone(), ver).await {
                Ok(Some(lease)) => {
                    cx.stats.record_make_transport_end_at();
                    if let Some(started) = fallback_start {
                        // A fallback candidate succeeded after earlier attempts failed/skipped.
                        // Record the transition so fallback usage is observable even on success.
                        tracing::debug!(
                            attempt,
                            candidate = %candidate,
                            elapsed_us = started.elapsed().as_micros(),
                            "[VOLO] dial plan fell back to a later candidate"
                        );
                    }
                    cx.extensions_mut()
                        .insert(SelectedTransport::new(candidate, attempt));
                    return Ok(AcquiredTransport { lease });
                }
                Ok(None) => {
                    // multiplex + shmipc candidate: skip and try the next candidate. Emit a
                    // tracing event now, because the skip reason is lost once a later candidate
                    // succeeds and the local summary is dropped.
                    tracing::debug!(
                        attempt,
                        candidate = %candidate,
                        "[VOLO] dial plan skipped candidate: shmipc does not support multiplex"
                    );
                    let _ = write!(
                        summary,
                        "; attempt {attempt} ({candidate}): skipped (shmipc does not support \
                         multiplex)"
                    );
                }
                Err(e) => {
                    // For a real multi-candidate plan, record each failed attempt (with its
                    // acquire latency): without it, a primary failure that is later masked by a
                    // successful fallback would leave no trace. For a plain single-address request
                    // (`n == 1`) we stay silent and let the error propagate, preserving the
                    // pre-existing log behavior and avoiding duplicate failure logs during outages.
                    if n > 1 {
                        // Reuse `make_transport_start_at` for the first attempt so the hot path
                        // never pays for an extra `Instant`; later attempts use their own timer.
                        let elapsed_us = match fallback_start {
                            Some(started) => started.elapsed().as_micros(),
                            None => cx
                                .stats
                                .make_transport_start_at()
                                .map(|start| {
                                    (chrono::Local::now() - start)
                                        .num_microseconds()
                                        .unwrap_or(0)
                                        .max(0) as u128
                                })
                                .unwrap_or(0),
                        };
                        tracing::debug!(
                            attempt,
                            candidate = %candidate,
                            elapsed_us,
                            error = %e,
                            "[VOLO] dial plan candidate failed"
                        );
                    }
                    let _ = write!(summary, "; attempt {attempt} ({candidate}): {e}");
                    last_err = Some(e);
                }
            }
        }

        // No candidate succeeded. `make_transport_end_at` stays unset to preserve the existing
        // "never acquired a transport" semantics.
        match last_err {
            Some(mut e) => {
                e.append_msg(&format!(", dial plan exhausted{summary}"));
                Err(e)
            }
            // Every candidate was skipped: a multiplex plan containing only shmipc candidates.
            None => Err(ClientError::Transport(
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "shmipc does not support multiplex",
                )
                .into(),
            )),
        }
    }

    /// Acquires a single candidate.
    ///
    /// Returns `Ok(Some(lease))` on success, `Ok(None)` if the candidate must be skipped (a shmipc
    /// candidate under multiplex), or `Err(_)` if acquiring the candidate failed.
    async fn acquire_candidate(
        &self,
        candidate: Address,
        ver: Ver,
    ) -> Result<Option<TransportLease<Address, MT::Response>>, ClientError> {
        #[cfg(feature = "shmipc")]
        if candidate.is_shmipc() {
            return match ver {
                // shmipc bypasses the outer pool: its SessionManager already pools streams.
                Ver::PingPong => self
                    .direct
                    .call(candidate)
                    .await
                    .map(|t| Some(TransportLease::Direct(t)))
                    .map_err(Into::into),
                // shmipc does not support multiplex; skip so a non-shmipc fallback can be used.
                Ver::Multiplex => Ok(None),
            };
        }

        self.pooled
            .call((candidate, ver))
            .await
            .map(|p| Some(TransportLease::Pooled(p)))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    use motore::service::UnaryService;
    use volo::{
        context::{Context, Role, RpcInfo},
        net::Address,
    };

    use super::{DialPlan, SelectedTransport, TransportAcquirer};
    use crate::{
        ClientError,
        context::{ClientContext, Config},
        protocol::TMessageType,
        transport::pool::{Poolable, Ver},
    };

    fn ip(port: u16) -> Address {
        Address::Ip(SocketAddr::from(([127, 0, 0, 1], port)))
    }

    #[cfg(feature = "shmipc")]
    fn shmipc(port: u16) -> Address {
        Address::Shmipc(volo::net::shmipc::Address::Tcp(SocketAddr::from((
            [127, 0, 0, 1],
            port,
        ))))
    }

    fn make_cx(address: Option<Address>) -> ClientContext {
        let mut cx = ClientContext::new(
            1,
            RpcInfo::<Config>::with_role(Role::Client),
            TMessageType::Call,
        );
        if let Some(addr) = address {
            cx.rpc_info_mut().callee_mut().set_address(addr);
        }
        cx
    }

    fn io_kind(e: &ClientError) -> Option<std::io::ErrorKind> {
        match e {
            ClientError::Transport(te) => Some(te.io_error().kind()),
            _ => None,
        }
    }

    /// Extracts the error from an acquire result whose `Ok` type is not `Debug`.
    fn expect_err<T>(res: Result<T, ClientError>) -> ClientError {
        match res {
            Ok(_) => panic!("expected an error, got Ok"),
            Err(e) => e,
        }
    }

    /// A minimal poolable transport for exercising the acquire algorithm.
    struct MockTransport;

    impl Poolable for MockTransport {
        async fn reusable(&self) -> bool {
            true
        }
    }

    /// A mock maker that records every address it is asked to dial and fails for a configured set.
    /// The fail set is mutable so a test can change it between successive acquires on one context.
    #[derive(Clone)]
    struct MockMaker {
        calls: Arc<Mutex<Vec<Address>>>,
        fail: Arc<Mutex<HashSet<Address>>>,
    }

    impl MockMaker {
        fn new(fail: impl IntoIterator<Item = Address>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail: Arc::new(Mutex::new(fail.into_iter().collect())),
            }
        }

        fn calls(&self) -> Vec<Address> {
            self.calls.lock().unwrap().clone()
        }

        /// Replaces the set of addresses that will fail on subsequent dials.
        fn set_fail(&self, fail: impl IntoIterator<Item = Address>) {
            *self.fail.lock().unwrap() = fail.into_iter().collect();
        }
    }

    impl UnaryService<Address> for MockMaker {
        type Response = MockTransport;
        type Error = std::io::Error;

        async fn call(&self, addr: Address) -> Result<Self::Response, Self::Error> {
            self.calls.lock().unwrap().push(addr.clone());
            if self.fail.lock().unwrap().contains(&addr) {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("mock fail for {addr}"),
                ))
            } else {
                Ok(MockTransport)
            }
        }
    }

    // ---- DialPlan / SelectedTransport pure logic ----

    #[test]
    fn dial_plan_single() {
        let plan = DialPlan::new(ip(1));
        assert_eq!(plan.primary(), &ip(1));
        assert_eq!(plan.attempts().len(), 1);
        assert_eq!(plan.attempts().cloned().collect::<Vec<_>>(), vec![ip(1)]);
    }

    #[test]
    fn dial_plan_with_fallback() {
        let plan = DialPlan::with_fallback(ip(1), ip(2));
        assert_eq!(plan.primary(), &ip(1));
        assert_eq!(
            plan.attempts().cloned().collect::<Vec<_>>(),
            vec![ip(1), ip(2)]
        );
    }

    #[test]
    fn dial_plan_dedup_identical_fallback() {
        let plan = DialPlan::with_fallback(ip(1), ip(1));
        assert_eq!(plan.attempts().len(), 1);
        assert_eq!(plan.primary(), &ip(1));
    }

    #[test]
    fn selected_transport_accessors() {
        let selected = SelectedTransport::new(ip(7), 3);
        assert_eq!(selected.address(), &ip(7));
        assert_eq!(selected.attempt(), 3);
    }

    // ---- acquire: validation ----

    #[tokio::test]
    async fn acquire_missing_address_is_invalid_data() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(None);
        // Even with a plan present, a missing callee address must fail fast.
        cx.extensions_mut().insert(DialPlan::new(ip(1)));

        let err = expect_err(acquirer.acquire(&mut cx, Ver::PingPong).await);
        assert_eq!(io_kind(&err), Some(std::io::ErrorKind::InvalidData));
        assert!(format!("{err}").contains("address is required"));
        assert!(maker.calls().is_empty(), "maker must not be called");
    }

    #[tokio::test]
    async fn acquire_plan_primary_mismatch_is_invalid_data() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(ip(1)));
        // Plan primary (ip(2)) disagrees with callee address (ip(1)).
        cx.extensions_mut()
            .insert(DialPlan::with_fallback(ip(2), ip(3)));

        let err = expect_err(acquirer.acquire(&mut cx, Ver::PingPong).await);
        assert_eq!(io_kind(&err), Some(std::io::ErrorKind::InvalidData));
        assert!(maker.calls().is_empty(), "no dial before validation");
    }

    // ---- acquire: candidate walk ----

    #[tokio::test]
    async fn acquire_single_fast_path_without_plan() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(ip(1)));

        acquirer.acquire(&mut cx, Ver::PingPong).await.unwrap();

        assert_eq!(maker.calls(), vec![ip(1)]);
        let selected = cx.extensions().get::<SelectedTransport>().unwrap();
        assert_eq!(selected.address(), &ip(1));
        assert_eq!(selected.attempt(), 0);
        assert!(cx.stats.make_transport_start_at().is_some());
        assert!(cx.stats.make_transport_end_at().is_some());
    }

    #[tokio::test]
    async fn acquire_primary_success_skips_fallback() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(ip(1)));
        cx.extensions_mut()
            .insert(DialPlan::with_fallback(ip(1), ip(2)));

        acquirer.acquire(&mut cx, Ver::PingPong).await.unwrap();

        assert_eq!(maker.calls(), vec![ip(1)], "fallback must not be dialed");
        let selected = cx.extensions().get::<SelectedTransport>().unwrap();
        assert_eq!(selected.address(), &ip(1));
        assert_eq!(selected.attempt(), 0);
    }

    #[tokio::test]
    async fn acquire_falls_back_on_primary_failure() {
        let maker = MockMaker::new([ip(1)]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(ip(1)));
        cx.extensions_mut()
            .insert(DialPlan::with_fallback(ip(1), ip(2)));

        acquirer.acquire(&mut cx, Ver::PingPong).await.unwrap();

        assert_eq!(maker.calls(), vec![ip(1), ip(2)]);
        let selected = cx.extensions().get::<SelectedTransport>().unwrap();
        assert_eq!(selected.address(), &ip(2));
        assert_eq!(selected.attempt(), 1);
        // callee address is rewritten to the actually selected fallback.
        assert_eq!(cx.rpc_info().callee().address(), Some(ip(2)));
        assert!(cx.stats.make_transport_end_at().is_some());
    }

    #[tokio::test]
    async fn acquire_all_fail_aggregates_errors() {
        let maker = MockMaker::new([ip(1), ip(2)]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(ip(1)));
        cx.extensions_mut()
            .insert(DialPlan::with_fallback(ip(1), ip(2)));

        let err = expect_err(acquirer.acquire(&mut cx, Ver::PingPong).await);
        let msg = format!("{err}");
        assert!(
            msg.contains("127.0.0.1:1"),
            "first attempt in summary: {msg}"
        );
        assert!(
            msg.contains("127.0.0.1:2"),
            "second attempt in summary: {msg}"
        );
        // callee address reflects the last attempt.
        assert_eq!(cx.rpc_info().callee().address(), Some(ip(2)));
        // end stays unset on total failure.
        assert!(cx.stats.make_transport_start_at().is_some());
        assert!(cx.stats.make_transport_end_at().is_none());
    }

    #[tokio::test]
    async fn acquire_failure_after_prior_success_clears_stale_selection() {
        // Model a retry layer reusing one context: first acquire succeeds on shmipc, the second
        // fails on every candidate. The stale shmipc SelectedTransport from the first attempt must
        // not linger, or downstream metrics would mislabel the failed UDS attempt as shmipc.
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(ip(1)));
        cx.extensions_mut()
            .insert(DialPlan::with_fallback(ip(1), ip(2)));

        // First acquire: primary succeeds.
        acquirer.acquire(&mut cx, Ver::PingPong).await.unwrap();
        assert_eq!(
            cx.extensions()
                .get::<SelectedTransport>()
                .map(|s| s.address().clone()),
            Some(ip(1))
        );

        // Second acquire on the same context: both candidates now fail.
        maker.set_fail([ip(1), ip(2)]);
        let _ = expect_err(acquirer.acquire(&mut cx, Ver::PingPong).await);

        // The stale selection is gone, so downstream reads fall back to the last attempted address.
        assert!(
            cx.extensions().get::<SelectedTransport>().is_none(),
            "stale SelectedTransport must be cleared on a fully-failed acquire"
        );
        assert_eq!(cx.rpc_info().callee().address(), Some(ip(2)));
    }

    #[tokio::test]
    async fn context_reset_clears_plan_and_selected() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(ip(1)));
        cx.extensions_mut().insert(DialPlan::new(ip(1)));
        acquirer.acquire(&mut cx, Ver::PingPong).await.unwrap();
        assert!(cx.extensions().get::<SelectedTransport>().is_some());

        cx.reset(2, TMessageType::Call);

        assert!(cx.extensions().get::<DialPlan>().is_none());
        assert!(cx.extensions().get::<SelectedTransport>().is_none());
    }

    // ---- shmipc / multiplex specific behavior ----

    #[cfg(feature = "shmipc")]
    #[tokio::test]
    async fn acquire_multiplex_skips_shmipc_candidate() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(shmipc(1)));
        cx.extensions_mut()
            .insert(DialPlan::with_fallback(shmipc(1), ip(2)));

        acquirer.acquire(&mut cx, Ver::Multiplex).await.unwrap();

        // shmipc candidate is skipped without ever calling the maker.
        assert_eq!(maker.calls(), vec![ip(2)]);
        let selected = cx.extensions().get::<SelectedTransport>().unwrap();
        assert_eq!(selected.address(), &ip(2));
        assert_eq!(selected.attempt(), 1);
    }

    #[cfg(feature = "shmipc")]
    #[tokio::test]
    async fn acquire_multiplex_only_shmipc_is_unsupported() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(shmipc(1)));
        cx.extensions_mut().insert(DialPlan::new(shmipc(1)));

        let err = expect_err(acquirer.acquire(&mut cx, Ver::Multiplex).await);
        assert_eq!(io_kind(&err), Some(std::io::ErrorKind::Unsupported));
        assert!(format!("{err}").contains("shmipc does not support multiplex"));
        assert!(maker.calls().is_empty(), "shmipc maker must not be called");
    }

    #[cfg(feature = "shmipc")]
    #[tokio::test]
    async fn acquire_pingpong_shmipc_uses_direct() {
        let maker = MockMaker::new([]);
        let acquirer = TransportAcquirer::new(maker.clone(), None);
        let mut cx = make_cx(Some(shmipc(1)));
        cx.extensions_mut()
            .insert(DialPlan::with_fallback(shmipc(1), ip(2)));

        acquirer.acquire(&mut cx, Ver::PingPong).await.unwrap();

        // pingpong dials the shmipc primary directly and succeeds without touching the fallback.
        assert_eq!(maker.calls(), vec![shmipc(1)]);
        let selected = cx.extensions().get::<SelectedTransport>().unwrap();
        assert_eq!(selected.address(), &shmipc(1));
        assert_eq!(selected.attempt(), 0);
    }
}
