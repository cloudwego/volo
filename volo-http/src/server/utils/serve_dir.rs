//! Service for serving a directory.
//!
//! This module includes [`ServeDir`], which can be used for serving a directory through a
//! catch-all uri like `/static/{*path}` or `Router::nest_service`.
//!
//! # Examples
//!
//! ```
//! use volo_http::server::{
//!     route::{get, Router},
//!     utils::ServeDir,
//! };
//!
//! let router: Router = Router::new()
//!     .route("/", get(|| async { "Hello, World" }))
//!     .nest_service("/static/", ServeDir::new("."));
//! ```
//!
//! The `"."` means `ServeDir` will serve the CWD (current working directory) and then you can
//! access any file in the directory.

use std::{
    fs,
    marker::PhantomData,
    path::{Path, PathBuf},
};

use http::{header::HeaderValue, status::StatusCode};
use motore::service::Service;

use super::FileResponse;
use crate::{
    context::ServerContext, request::ServerRequest, response::ServerResponse, server::IntoResponse,
};

/// [`ServeDir`] is a service for sending files from a given directory.
pub struct ServeDir<E, F> {
    path: PathBuf,
    mime_getter: F,
    _marker: PhantomData<fn(E)>,
}

impl<E> ServeDir<E, fn(&Path) -> HeaderValue> {
    /// Create a new [`ServeDir`] service with the given path.
    ///
    /// # Panics
    ///
    /// - Panics if the path is invalid
    /// - Panics if the path is not a directory
    pub fn new<P>(path: P) -> Self
    where
        P: AsRef<Path>,
    {
        let path = fs::canonicalize(path).expect("ServeDir: failed to canonicalize path");
        assert!(path.is_dir());
        Self {
            path,
            mime_getter: guess_mime,
            _marker: PhantomData,
        }
    }

    /// Set a function for getting mime from file path.
    ///
    /// By default, [`ServeDir`] will use `mime_guess` crate for guessing a mime through the file
    /// extension name.
    pub fn mime_getter<F>(self, mime_getter: F) -> ServeDir<E, F>
    where
        F: Fn(&Path) -> HeaderValue,
    {
        ServeDir {
            path: self.path,
            mime_getter,
            _marker: self._marker,
        }
    }
}

impl<B, E, F> Service<ServerContext, ServerRequest<B>> for ServeDir<E, F>
where
    B: Send,
    F: Fn(&Path) -> HeaderValue + Sync,
{
    type Response = ServerResponse;
    type Error = E;

    async fn call(
        &self,
        _: &mut ServerContext,
        req: ServerRequest<B>,
    ) -> Result<Self::Response, Self::Error> {
        // Get relative path from uri
        let path = req.uri().path();
        let path = path.strip_prefix('/').unwrap_or(path);

        tracing::trace!("[Volo-HTTP] ServeDir: path: {path}");

        // Join to the serving directory and canonicalize it
        let path = self.path.join(path);
        let Ok(path) = fs::canonicalize(path) else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        };

        // Reject file which is out of the serving directory
        if path.strip_prefix(self.path.as_path()).is_err() {
            tracing::debug!("[Volo-HTTP] ServeDir: illegal path: {}", path.display());
            return Ok(StatusCode::FORBIDDEN.into_response());
        }

        // Check metadata and permission
        if !path.is_file() {
            return Ok(StatusCode::NOT_FOUND.into_response());
        }

        // Get mime and return it!
        let content_type = (self.mime_getter)(&path);
        let Ok(resp) = FileResponse::new(path, content_type) else {
            return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
        };
        Ok(resp.into_response())
    }
}

pub fn guess_mime(path: &Path) -> HeaderValue {
    mime_guess::from_path(path)
        .first_raw()
        .map(HeaderValue::from_static)
        .unwrap_or_else(|| HeaderValue::from_str(mime::APPLICATION_OCTET_STREAM.as_ref()).unwrap())
}

#[cfg(test)]
mod serve_dir_tests {
    use http::{method::Method, StatusCode};

    use super::ServeDir;
    use crate::{
        body::Body,
        server::{Router, Server},
    };

    #[tokio::test]
    async fn read_file() {
        // volo/volo-http
        let router: Router<Option<Body>> =
            Router::new().nest_service("/static/", ServeDir::new("."));
        let server = Server::new(router).into_test_server();
        // volo/volo-http/Cargo.toml
        assert!(server
            .call_route(Method::GET, "/static/Cargo.toml", None)
            .await
            .status()
            .is_success());
        // volo/volo-http/src/lib.rs
        assert!(server
            .call_route(Method::GET, "/static/src/lib.rs", None)
            .await
            .status()
            .is_success());
        // volo/volo-http/Cargo.lock, this file does not exist
        assert_eq!(
            server
                .call_route(Method::GET, "/static/Cargo.lock", None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        // volo/Cargo.toml, this file should be rejected
        assert_eq!(
            server
                .call_route(Method::GET, "/static/../Cargo.toml", None)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
