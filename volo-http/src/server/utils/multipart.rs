//! Multipart implementation for server.
//!
//! This module provides utilities for extracting `multipart/form-data` formatted data from HTTP
//! requests.
//!
//! # Example
//!
//! ```rust
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
//! async fn upload(mut multipart: Multipart) -> Result<StatusCode, MultipartRejectionError> {
//!     while let Some(field) = multipart.next_field().await? {
//!         let name = field.name().unwrap().to_string();
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

use std::{error::Error, fmt};

use http::{request::Parts, StatusCode};
use http_body_util::BodyExt;
use multer::Field;

use crate::{
    context::ServerContext,
    server::{extract::FromRequest, IntoResponse},
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
/// ```rust
/// use http::StatusCode;
/// use volo_http::{
///     response::ServerResponse,
///     server::utils::multipart::{Multipart, MultipartRejectionError},
/// };
///
/// async fn upload(mut multipart: Multipart) -> Result<StatusCode, MultipartRejectionError> {
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
/// Since the body is unlimited, so it is recommended to use
/// [`BodyLimitLayer`](crate::server::layer::BodyLimitLayer) to limit the size of the body.
///
/// ```rust
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
/// # async fn upload_handler(mut multipart: Multipart) -> Result<StatusCode, MultipartRejectionError> {
/// # Ok(StatusCode::OK)
/// # }
///
/// let app: Router<_>= Router::new()
///     .route("/",post(upload_handler))
///     .layer(BodyLimitLayer::new(1024));
/// ```
#[must_use]
pub struct Multipart {
    inner: multer::Multipart<'static>,
}

impl Multipart {
    /// Iterate over all [`Field`] in [`Multipart`]
    ///
    /// # Example
    ///
    /// ```rust
    /// # use volo_http::server::utils::multipart::Multipart;
    /// # let mut multipart: Multipart;
    /// // Extract each field from multipart by using while loop
    /// # async fn upload(mut multipart: Multipart) {
    /// while let Some(field) = multipart.next_field().await.unwrap() {
    ///     let name = field.name().unwrap().to_string(); // Get field name
    ///     let data = field.bytes().await.unwrap(); // Get field data
    /// }
    /// # }
    /// ```
    pub async fn next_field(&mut self) -> Result<Option<Field<'static>>, MultipartRejectionError> {
        Ok(self.inner.next_field().await?)
    }
}

impl FromRequest<crate::body::Body> for Multipart {
    type Rejection = MultipartRejectionError;
    async fn from_request(
        _: &mut ServerContext,
        parts: Parts,
        body: crate::body::Body,
    ) -> Result<Self, Self::Rejection> {
        let boundary = multer::parse_boundary(
            parts
                .headers
                .get(http::header::CONTENT_TYPE)
                .ok_or(multer::Error::NoMultipart)?
                .to_str()
                .map_err(|_| multer::Error::NoBoundary)?,
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
        multer::Error::StreamReadFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
        self.to_status_code().into_response()
    }
}

#[cfg(test)]
mod multipart_tests {
    use std::{
        convert::Infallible,
        net::{IpAddr, Ipv4Addr, SocketAddr},
    };

    use motore::Service;
    use reqwest::multipart::Form;
    use volo::net::Address;

    use crate::{
        context::ServerContext,
        request::ServerRequest,
        response::ServerResponse,
        server::{
            test_helpers,
            utils::multipart::{Multipart, MultipartRejectionError},
            IntoResponse,
        },
        Server,
    };

    fn _test_compile() {
        async fn handler(_: Multipart) {}
        let app = test_helpers::to_service(handler);
        let addr = Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            25241,
        ));
        let _server = Server::new(app).run(addr);
    }

    async fn run_handler<S>(service: S, port: u16)
    where
        S: Service<ServerContext, ServerRequest, Response=ServerResponse, Error=Infallible>
        + Send
        + Sync
        + 'static,
    {
        let addr = Address::Ip(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port,
        ));

        tokio::spawn(Server::new(service).run(addr));

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn test_single_field_upload() {
        const BYTES: &[u8] = "<!doctype html><title>🦀</title>".as_bytes();
        const FILE_NAME: &str = "index.html";
        const CONTENT_TYPE: &str = "text/html; charset=utf-8";

        async fn handler(mut multipart: Multipart) -> impl IntoResponse {
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

        run_handler(test_helpers::to_service(handler), 25241).await;

        let url_str = format!("http://127.0.0.1:{}", 25241);
        let url = url::Url::parse(url_str.as_str()).unwrap();

        reqwest::Client::new()
            .post(url)
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

        async fn handler(mut multipart: Multipart) -> Result<(), MultipartRejectionError> {
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

        let form = Form::new().part(
            FIELD_NAME1,
            reqwest::multipart::Part::bytes(BYTES)
                .file_name(FILE_NAME1)
                .mime_str(CONTENT_TYPE)
                .unwrap()
                .headers(reqwest::header::HeaderMap::from_iter([(
                    reqwest::header::HeaderName::from_static("foo1"),
                    reqwest::header::HeaderValue::from_static("bar1"),
                )])),
        ).part(
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

        run_handler(test_helpers::to_service(handler), 25242).await;

        let url_str = format!("http://127.0.0.1:{}", 25242);
        let url = url::Url::parse(url_str.as_str()).unwrap();

        reqwest::Client::new()
            .post(url.clone())
            .multipart(form)
            .send()
            .await
            .unwrap();
    }
}
