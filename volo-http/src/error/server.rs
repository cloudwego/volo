use std::{error::Error, fmt};

use http::StatusCode;

use crate::{response::ServerResponse, server::IntoResponse};

#[derive(Debug)]
#[non_exhaustive]
pub enum ExtractBodyError {
    Common(CommonRejectionError),
    String(simdutf8::basic::Utf8Error),
    #[cfg(feature = "json")]
    Json(crate::json::Error),
    #[cfg(feature = "form")]
    Form(serde_urlencoded::de::Error),
}

impl fmt::Display for ExtractBodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("failed to extract ")?;
        match self {
            Self::Common(e) => write!(f, "data: {e}"),
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
            Self::Common(e) => e.to_status_code(),
            Self::String(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            #[cfg(feature = "json")]
            Self::Json(_) => StatusCode::BAD_REQUEST,
            #[cfg(feature = "form")]
            Self::Form(_) => StatusCode::BAD_REQUEST,
        };

        status.into_response()
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum CommonRejectionError {
    BodyCollectionError,
    InvalidContentType,
}

impl fmt::Display for CommonRejectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BodyCollectionError => write!(f, "failed to collect the response body"),
            Self::InvalidContentType => write!(f, "invalid content type"),
        }
    }
}

impl Error for CommonRejectionError {}

impl CommonRejectionError {
    pub fn to_status_code(self) -> StatusCode {
        match self {
            Self::BodyCollectionError => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InvalidContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        }
    }
}

impl IntoResponse for CommonRejectionError {
    fn into_response(self) -> ServerResponse {
        self.to_status_code().into_response()
    }
}

pub fn body_collection_error() -> ExtractBodyError {
    ExtractBodyError::Common(CommonRejectionError::BodyCollectionError)
}

pub fn invalid_content_type() -> ExtractBodyError {
    ExtractBodyError::Common(CommonRejectionError::InvalidContentType)
}
