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
    Json(crate::utils::json::Error),
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
    pub fn to_status_code(&self) -> StatusCode {
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
