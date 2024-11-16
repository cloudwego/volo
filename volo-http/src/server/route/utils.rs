use std::{
    collections::{hash_map::Drain, HashMap},
    error::Error,
    fmt,
    future::Future,
    str::FromStr,
};

use http::uri::Uri;
use motore::{layer::Layer, service::Service};

use crate::{context::ServerContext, request::Request, response::Response};

// The `matchit::Router` cannot be converted to `Iterator`, so using
// `matchit::Router<MethodRouter>` is not convenient enough.
//
// To solve the problem, we refer to the implementation of `axum` and introduce a `RouteId` as a
// bridge, the `matchit::Router` only handles some IDs and each ID corresponds to a `MethodRouter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct RouteId(u32);

impl RouteId {
    fn next() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
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
pub(super) struct Matcher {
    matches: HashMap<String, RouteId>,
    router: matchit::Router<RouteId>,
}

impl Matcher {
    pub fn insert<R>(&mut self, uri: R) -> Result<RouteId, MatcherError>
    where
        R: Into<String>,
    {
        let route_id = RouteId::next();
        self.insert_with_id(uri, route_id)?;
        Ok(route_id)
    }

    pub fn insert_with_id<R>(&mut self, uri: R, route_id: RouteId) -> Result<(), MatcherError>
    where
        R: Into<String>,
    {
        let uri = uri.into();
        if self.matches.insert(uri.clone(), route_id).is_some() {
            return Err(MatcherError::UriConflict(uri));
        }
        self.router
            .insert(uri, route_id)
            .map_err(MatcherError::RouterInsertError)?;
        Ok(())
    }

    pub fn at<'a>(
        &'a self,
        path: &'a str,
    ) -> Result<matchit::Match<'a, 'a, &'a RouteId>, MatcherError> {
        self.router.at(path).map_err(MatcherError::RouterMatchError)
    }

    pub fn drain(&mut self) -> Drain<String, RouteId> {
        self.matches.drain()
    }
}

#[derive(Debug)]
pub(super) enum MatcherError {
    UriConflict(String),
    RouterInsertError(matchit::InsertError),
    RouterMatchError(matchit::MatchError),
}

impl fmt::Display for MatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UriConflict(uri) => write!(f, "URI conflict: {uri}"),
            Self::RouterInsertError(err) => write!(f, "router insert error: {err}"),
            Self::RouterMatchError(err) => write!(f, "router match error: {err}"),
        }
    }
}

impl Error for MatcherError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UriConflict(_) => None,
            Self::RouterInsertError(e) => Some(e),
            Self::RouterMatchError(e) => Some(e),
        }
    }
}

pub(super) struct StripPrefixLayer;

impl<S> Layer<S> for StripPrefixLayer {
    type Service = StripPrefix<S>;

    fn layer(self, inner: S) -> Self::Service {
        StripPrefix { inner }
    }
}

pub(super) const NEST_CATCH_PARAM: &str = "{*__priv_nest_catch_param}";
pub(super) const NEST_CATCH_PARAM_NAME: &str = "__priv_nest_catch_param";

pub(super) struct StripPrefix<S> {
    inner: S,
}

impl<S, B, E> Service<ServerContext, Request<B>> for StripPrefix<S>
where
    S: Service<ServerContext, Request<B>, Response = Response, Error = E>,
{
    type Response = Response;
    type Error = E;

    fn call(
        &self,
        cx: &mut ServerContext,
        mut req: Request<B>,
    ) -> impl Future<Output = Result<Self::Response, Self::Error>> + Send {
        let mut uri = String::from("/");
        if cx
            .params()
            .last()
            .is_some_and(|(k, _)| k == NEST_CATCH_PARAM_NAME)
        {
            uri += cx.params_mut().pop().unwrap().1.as_str();
        };
        if let Some(query) = req.uri().query() {
            uri.push('?');
            uri.push_str(query);
        }

        // SAFETY: The value is from a valid uri, so it can also be converted into
        // a valid uri safely.
        *req.uri_mut() = Uri::from_str(&uri).expect("infallible: stripped uri is invalid");
        self.inner.call(cx, req)
    }
}
