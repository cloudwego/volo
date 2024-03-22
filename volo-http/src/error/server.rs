use http::StatusCode;

use crate::{response::ServerResponse, server::IntoResponse};

#[derive(Debug)]
#[non_exhaustive]
pub enum RejectionError {
    Common(CommonRejectionError),
    String(simdutf8::basic::Utf8Error),
    #[cfg(feature = "__json")]
    Json(crate::json::Error),
    #[cfg(feature = "query")]
    Query(serde_urlencoded::de::Error),
    #[cfg(feature = "form")]
    Form(serde_html_form::de::Error),
}

impl IntoResponse for RejectionError {
    fn into_response(self) -> ServerResponse {
        let status = match self {
            Self::Common(e) => e.to_status_code(),
            Self::String(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            #[cfg(feature = "__json")]
            Self::Json(_) => StatusCode::BAD_REQUEST,
            #[cfg(feature = "query")]
            Self::Query(_) => StatusCode::BAD_REQUEST,
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

pub fn body_collection_error() -> RejectionError {
    RejectionError::Common(CommonRejectionError::BodyCollectionError)
}

pub fn invalid_content_type() -> RejectionError {
    RejectionError::Common(CommonRejectionError::InvalidContentType)
}
