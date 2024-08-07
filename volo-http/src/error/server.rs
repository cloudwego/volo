//! Generic error types for server

use std::{error::Error, fmt};

use http::StatusCode;

use crate::{response::ServerResponse, server::IntoResponse};

/// [`Error`]s when extracting something from a [`Body`](crate::body::Body)
#[derive(Debug)]
#[non_exhaustive]
pub enum ExtractBodyError {
    /// Generic extracting errors when pulling [`Body`](crate::body::Body) or checking
    /// `Content-Type` of request
    Generic(GenericRejectionError),
    /// The [`Body`](crate::body::Body) cannot be extracted as a [`String`] or
    /// [`FastStr`](faststr::FastStr)
    String(simdutf8::basic::Utf8Error),
    /// The [`Body`](crate::body::Body) cannot be extracted as a json object
    #[cfg(feature = "json")]
    Json(crate::json::Error),
    /// The [`Body`](crate::body::Body) cannot be extracted as a form object
    #[cfg(feature = "form")]
    Form(serde_urlencoded::de::Error),
}

impl fmt::Display for ExtractBodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("failed to extract ")?;
        match self {
            Self::Generic(e) => write!(f, "data: {e}"),
            Self::String(e) => write!(f, "string: {e}"),
            #[cfg(feature = "json")]
            Self::Json(e) => write!(f, "json: {e}"),
            #[cfg(feature = "form")]
            Self::Form(e) => write!(f, "form: {e}"),
        }
    }
}

impl IntoResponse for ExtractBodyError {
    fn into_response(self) -> ServerResponse {
        let status = match self {
            Self::Generic(e) => e.to_status_code(),
            Self::String(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            #[cfg(feature = "json")]
            Self::Json(_) => StatusCode::BAD_REQUEST,
            #[cfg(feature = "form")]
            Self::Form(_) => StatusCode::BAD_REQUEST,
        };

        status.into_response()
    }
}

/// Generic rejection [`Error`]s
#[derive(Debug)]
#[non_exhaustive]
pub enum GenericRejectionError {
    /// Failed to collect the [`Body`](crate::body::Body)
    BodyCollectionError,
    /// The `Content-Type` is invalid for the extractor
    InvalidContentType,
}

impl fmt::Display for GenericRejectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyCollectionError => write!(f, "failed to collect the response body"),
            Self::InvalidContentType => write!(f, "invalid content type"),
        }
    }
}

impl Error for GenericRejectionError {}

impl GenericRejectionError {
    /// Convert the [`GenericRejectionError`] to the corresponding [`StatusCode`]
    pub fn to_status_code(self) -> StatusCode {
        match self {
            Self::BodyCollectionError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InvalidContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        }
    }
}

impl IntoResponse for GenericRejectionError {
    fn into_response(self) -> ServerResponse {
        self.to_status_code().into_response()
    }
}

/// Create a generic [`ExtractBodyError`] with [`GenericRejectionError::BodyCollectionError`]
pub fn body_collection_error() -> ExtractBodyError {
    ExtractBodyError::Generic(GenericRejectionError::BodyCollectionError)
}

/// Create a generic [`ExtractBodyError`] with [`GenericRejectionError::InvalidContentType`]
pub fn invalid_content_type() -> ExtractBodyError {
    ExtractBodyError::Generic(GenericRejectionError::InvalidContentType)
}

/// Rejection used for [`WebSocketUpgrade`](crate::server::utils::WebSocketUpgrade).
#[derive(Debug)]
#[non_exhaustive]
pub enum WebSocketUpgradeRejectionError {
    /// The request method must be `GET`
    MethodNotGet,
    /// The HTTP version is not supported
    InvalidHttpVersion,
    /// The `Connection` header is invalid
    InvalidConnectionHeader,
    /// The `Upgrade` header is invalid
    InvalidUpgradeHeader,
    /// The `Sec-WebSocket-Version` header is invalid
    InvalidWebSocketVersionHeader,
    /// The `Sec-WebSocket-Key` header is missing
    WebSocketKeyHeaderMissing,
    /// The connection is not upgradable
    ConnectionNotUpgradable,
}

impl WebSocketUpgradeRejectionError {
    /// Convert the [`WebSocketUpgradeRejectionError`] to the corresponding [`StatusCode`]
    pub fn to_status_code(self) -> StatusCode {
        match self {
            Self::MethodNotGet => StatusCode::METHOD_NOT_ALLOWED,
            Self::InvalidHttpVersion => StatusCode::HTTP_VERSION_NOT_SUPPORTED,
            Self::InvalidConnectionHeader => StatusCode::BAD_REQUEST,
            Self::InvalidUpgradeHeader => StatusCode::BAD_REQUEST,
            Self::InvalidWebSocketVersionHeader => StatusCode::BAD_REQUEST,
            Self::WebSocketKeyHeaderMissing => StatusCode::BAD_REQUEST,
            Self::ConnectionNotUpgradable => StatusCode::UPGRADE_REQUIRED,
        }
    }
}

impl Error for WebSocketUpgradeRejectionError {}

impl fmt::Display for WebSocketUpgradeRejectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MethodNotGet => write!(f, "Request method must be 'GET'"),
            Self::InvalidHttpVersion => {
                write!(f, "Http version not support, only support HTTP 1.1 for now")
            }
            Self::InvalidConnectionHeader => {
                write!(f, "Connection header did not include 'upgrade'")
            }
            Self::InvalidUpgradeHeader => write!(f, "`Upgrade` header did not include 'websocket'"),
            Self::InvalidWebSocketVersionHeader => {
                write!(f, "`Sec-WebSocket-Version` header did not include '13'")
            }
            Self::WebSocketKeyHeaderMissing => write!(f, "`Sec-WebSocket-Key` header missing"),
            Self::ConnectionNotUpgradable => write!(
                f,
                "WebSocket request couldn't be upgraded since no upgrade state was present"
            ),
        }
    }
}

impl IntoResponse for WebSocketUpgradeRejectionError {
    fn into_response(self) -> ServerResponse {
        self.to_status_code().into_response()
    }
}
