//! Multipart implementation for server.
//!
//! This module provides utilities for extracting `multipart/form-data` formatted data from HTTP
//! requests.
//!
//! # Example
//!
//! ```rust,no_run
//! use http::StatusCode;
//! use volo_http::{
//!     response::ServerResponse,
//!     server::{
//!         route::post,
//!         utils::multipart::{Multipart, MultipartRejectionError},
//!     },
//!     Router,
//! };
//!
//! async fn upload(
//!     mut multipart: Multipart<'static>,
//! ) -> Result<StatusCode, MultipartRejectionError> {
//!     while let Some(field) = multipart.next_field().await? {
//!         let name = field.name().unwrap();
//!         let value = field.bytes().await?;
//!
//!         println!("The field {} has {} bytes", name, value.len());
//!     }
//!
//!     Ok(StatusCode::OK)
//! }
//!
//! let app: Router = Router::new().route("/upload", post(upload));
//! ```
//!
//! See [`Multipart`] for more details.

use std::{error::Error, fmt, fmt::Debug};

use http::{request::Parts, StatusCode};
use http_body_util::BodyExt;
use multer::Field;

use crate::{
    context::ServerContext,
    server::{extract::FromRequest, layer::body_limit::BodyLimitKind, IntoResponse},
};

/// Extract a type from `multipart/form-data` HTTP requests.
///
/// [`Multipart`] can be passed as an argument to a handler, which can be used to extract each
/// `multipart/form-data` field by calling [`Multipart::next_field`].
///
/// **Notice**
///
/// Extracting `multipart/form-data` data will consume the body, hence [`Multipart`] must be the
/// last argument from the handler.
///
/// # Example
///
/// ```rust,no_run
/// use http::StatusCode;
/// use volo_http::{
///     response::ServerResponse,
///     server::utils::multipart::{Multipart, MultipartRejectionError},
/// };
///
/// async fn upload(
///     mut multipart: Multipart<'static>,
/// ) -> Result<StatusCode, MultipartRejectionError> {
///     while let Some(field) = multipart.next_field().await? {
///         todo!()
///     }
///
///     Ok(StatusCode::OK)
/// }
/// ```
///
/// # Body Limitation
///
/// Since the body is unlimited, so it is recommended to use [`BodyLimitLayer`](crate::server::layer::BodyLimitLayer) to limit the size of
/// the body.
///
/// ```rust,no_run
/// use http::StatusCode;
/// use volo_http::{
///     Router,
///     server::{
///         layer::BodyLimitLayer,
///         route::post,
///         utils::multipart::{Multipart, MultipartRejectionError},
///     }
/// };
///
/// # async fn upload_handler(mut multipart: Multipart<'static>) -> Result<StatusCode, MultipartRejectionError> {
/// # Ok(StatusCode::OK)
/// # }
///
/// let app: Router<_>= Router::new()
///     .route("/",post(upload_handler))
///     .layer( BodyLimitLayer::max(1024));
/// ```
#[must_use]
pub struct Multipart<'r> {
    inner: multer::Multipart<'r>,
}

impl<'r> Multipart<'r> {
    /// Iterate over all [`Field`] in [`Multipart`]
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use volo_http::server::utils::multipart::Multipart;
    /// # let mut multipart: Multipart;
    /// // Extract each field from multipart by using while loop
    /// # async {
    ///  while let Some(field) = multipart.next_field().await? {
    ///     let name = field.name().unwrap().to_string(); // Get field name
    ///     let data = field.bytes().await?; // Get field data
    ///  }
    /// # }
    /// ```
    pub async fn next_field(&mut self) -> Result<Option<Field<'r>>, MultipartRejectionError> {
        let field = self.inner.next_field().await?;

        if let Some(field) = field {
            Ok(Some(field))
        } else {
            Ok(None)
        }
    }
}

impl<'r> FromRequest<crate::body::Body> for Multipart<'r> {
    type Rejection = MultipartRejectionError;
    async fn from_request(
        _: &mut ServerContext,
        parts: Parts,
        body: crate::body::Body,
    ) -> Result<Self, Self::Rejection> {
        let body = match parts.extensions.get::<BodyLimitKind>().copied() {
            Some(BodyLimitKind::Disable) => body,
            Some(BodyLimitKind::Limit(limit)) => {
                crate::body::Body::from_body(http_body_util::Limited::new(body, limit))
            }
            None => body,
        };

        let boundary = multer::parse_boundary(
            parts
                .headers
                .get(http::header::CONTENT_TYPE)
                .map(|h| h.to_str().unwrap_or_default())
                .unwrap_or_default(),
        )?;

        let multipart = multer::Multipart::new(body.into_data_stream(), boundary);

        Ok(Self { inner: multipart })
    }
}

/// [`Error`]s while extracting [`Multipart`].
///
/// [`Error`]: Error
#[derive(Debug)]
pub struct MultipartRejectionError {
    inner: multer::Error,
}

impl From<multer::Error> for MultipartRejectionError {
    fn from(err: multer::Error) -> Self {
        Self { inner: err }
    }
}

fn status_code_from_multer_error(err: &multer::Error) -> StatusCode {
    match err {
        multer::Error::UnknownField { .. }
        | multer::Error::IncompleteFieldData { .. }
        | multer::Error::IncompleteHeaders
        | multer::Error::ReadHeaderFailed(..)
        | multer::Error::DecodeHeaderName { .. }
        | multer::Error::DecodeContentType(..)
        | multer::Error::NoBoundary
        | multer::Error::DecodeHeaderValue { .. }
        | multer::Error::NoMultipart
        | multer::Error::IncompleteStream => StatusCode::BAD_REQUEST,
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            StatusCode::PAYLOAD_TOO_LARGE
        }
        multer::Error::StreamReadFailed(err) => {
            if let Some(err) = err.downcast_ref::<multer::Error>() {
                return status_code_from_multer_error(err);
            }

            if err
                .downcast_ref::<http_body_util::LengthLimitError>()
                .is_some()
            {
                return StatusCode::PAYLOAD_TOO_LARGE;
            }

            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl MultipartRejectionError {
    /// Convert the [`MultipartRejectionError`] into a [`http::StatusCode`].
    pub fn to_status_code(&self) -> http::StatusCode {
        status_code_from_multer_error(&self.inner)
    }
}

impl Error for MultipartRejectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.inner)
    }
}

impl fmt::Display for MultipartRejectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl IntoResponse for MultipartRejectionError {
    fn into_response(self) -> http::Response<crate::body::Body> {
        (self.to_status_code(), self.to_string()).into_response()
    }
}

#[cfg(test)]
mod multipart_tests {
    use std::{
        convert::Infallible,
        net::{IpAddr, Ipv4Addr, SocketAddr},
    };

    use motore::Service;
    use rand::Rng;
    use reqwest::multipart::Form;
    use volo::net::Address;

    use crate::{
        context::ServerContext,
        request::ServerRequest,
        response::ServerResponse,
        server::{
            layer::BodyLimitLayer,
            route::post,
            test_helpers,
            utils::multipart::{Multipart, MultipartRejectionError},
            IntoResponse,
        },
        Router, Server,
    };

    fn _test_compile() {
        async fn handler(_: Multipart<'_>) -> Result<(), Infallible> {
            Ok(())
        }
        let app = test_helpers::to_service(handler);
        let addr = Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8001,
        ));
        let _server = Server::new(app).run(addr);
    }

    async fn run_handler<S>(service: S, port: u16)
    where
        S: Service<ServerContext, ServerRequest, Response = ServerResponse, Error = Infallible>
            + Send
            + Sync
            + 'static,
    {
        let addr = Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port,
        ));

        tokio::spawn(Server::new(service).run(addr.clone()));

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    #[tokio::test]
    async fn test_single_field_upload() {
        const BYTES: &[u8] = "<!doctype html><title>🦀</title>".as_bytes();
        const FILE_NAME: &str = "index.html";
        const CONTENT_TYPE: &str = "text/html; charset=utf-8";

        async fn handler(mut multipart: Multipart<'static>) -> impl IntoResponse {
            let field = multipart.next_field().await.unwrap().unwrap();

            assert_eq!(field.file_name().unwrap(), FILE_NAME);
            assert_eq!(field.content_type().unwrap().as_ref(), CONTENT_TYPE);
            assert_eq!(field.headers()["foo"], "bar");
            assert_eq!(field.bytes().await.unwrap(), BYTES);

            assert!(multipart.next_field().await.unwrap().is_none());
        }

        let form = Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(BYTES)
                .file_name(FILE_NAME)
                .mime_str(CONTENT_TYPE)
                .unwrap()
                .headers(reqwest::header::HeaderMap::from_iter([(
                    reqwest::header::HeaderName::from_static("foo"),
                    reqwest::header::HeaderValue::from_static("bar"),
                )])),
        );

        run_handler(test_helpers::to_service(handler), 8001).await;

        let url_str = format!("http://127.0.0.1:{}", 8001);
        let url = url::Url::parse(url_str.as_str()).unwrap();

        reqwest::Client::new()
            .post(url.clone())
            .multipart(form)
            .send()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_multiple_field_upload() {
        const BYTES: &[u8] = "<!doctype html><title>🦀</title>".as_bytes();
        const CONTENT_TYPE: &str = "text/html; charset=utf-8";

        const FIELD_NAME1: &str = "file1";
        const FIELD_NAME2: &str = "file2";
        const FILE_NAME1: &str = "index1.html";
        const FILE_NAME2: &str = "index2.html";

        async fn handler(mut multipart: Multipart<'static>) -> Result<(), MultipartRejectionError> {
            while let Some(field) = multipart.next_field().await? {
                match field.name() {
                    Some(FIELD_NAME1) => {
                        assert_eq!(field.file_name().unwrap(), FILE_NAME1);
                        assert_eq!(field.headers()["foo1"], "bar1");
                    }
                    Some(FIELD_NAME2) => {
                        assert_eq!(field.file_name().unwrap(), FILE_NAME2);
                        assert_eq!(field.headers()["foo2"], "bar2");
                    }
                    _ => unreachable!(),
                }
                assert_eq!(field.content_type().unwrap().as_ref(), CONTENT_TYPE);
                assert_eq!(field.bytes().await?, BYTES);
            }

            Ok(())
        }

        let form1 = Form::new().part(
            FIELD_NAME1,
            reqwest::multipart::Part::bytes(BYTES)
                .file_name(FILE_NAME1)
                .mime_str(CONTENT_TYPE)
                .unwrap()
                .headers(reqwest::header::HeaderMap::from_iter([(
                    reqwest::header::HeaderName::from_static("foo1"),
                    reqwest::header::HeaderValue::from_static("bar1"),
                )])),
        );
        let form2 = Form::new().part(
            FIELD_NAME2,
            reqwest::multipart::Part::bytes(BYTES)
                .file_name(FILE_NAME2)
                .mime_str(CONTENT_TYPE)
                .unwrap()
                .headers(reqwest::header::HeaderMap::from_iter([(
                    reqwest::header::HeaderName::from_static("foo2"),
                    reqwest::header::HeaderValue::from_static("bar2"),
                )])),
        );

        run_handler(test_helpers::to_service(handler), 8002).await;

        let url_str = format!("http://127.0.0.1:{}", 8002);
        let url = url::Url::parse(url_str.as_str()).unwrap();

        for form in vec![form1, form2] {
            reqwest::Client::new()
                .post(url.clone())
                .multipart(form)
                .send()
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn test_large_field_upload() {
        async fn handler(mut multipart: Multipart<'static>) -> Result<(), MultipartRejectionError> {
            while let Some(field) = multipart.next_field().await? {
                field.bytes().await?;
            }

            Ok(())
        }

        // generate random bytes
        let mut rng = rand::thread_rng();
        let min_part_size = 4096;
        let mut body = vec![0; min_part_size];
        rng.fill(&mut body[..]);

        let content_type = "text/html; charset=utf-8";
        let field_name = "file";
        let file_name = "index.html";

        let form = Form::new().part(
            field_name,
            reqwest::multipart::Part::bytes(body)
                .file_name(file_name)
                .mime_str(content_type)
                .unwrap(),
        );

        let app: Router<_> = Router::new()
            .route("/", post(handler))
            .layer(BodyLimitLayer::max(1024));

        run_handler(app, 8003).await;

        let url_str = format!("http://127.0.0.1:{}", 8003);
        let url = url::Url::parse(url_str.as_str()).unwrap();

        let resp = reqwest::Client::new()
            .post(url.clone())
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    }
}
