use bytes::Bytes;
use http::{
    header::{self, HeaderMap},
    request::Parts,
    StatusCode,
};
use hyper::body::Incoming;
use serde::de::DeserializeOwned;

use super::{deserialize, Error, Json};
use crate::{
    context::ServerContext,
    response::ServerResponse,
    server::{
        extract::{FromRequest, RejectionError},
        IntoResponse,
    },
};

impl IntoResponse for Error {
    fn into_response(self) -> ServerResponse {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}

impl<T> FromRequest for Json<T>
where
    T: DeserializeOwned,
{
    type Rejection = RejectionError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        if !json_content_type(&parts.headers) {
            return Err(RejectionError::InvalidContentType);
        }

        let bytes = Bytes::from_request(cx, parts, body).await?;
        let json = deserialize(&bytes).map_err(RejectionError::JsonRejection)?;

        Ok(Json(json))
    }
}

fn json_content_type(headers: &HeaderMap) -> bool {
    let content_type = if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        content_type
    } else {
        return false;
    };

    let content_type = if let Ok(content_type) = content_type.to_str() {
        content_type
    } else {
        return false;
    };

    let mime = if let Ok(mime) = content_type.parse::<mime::Mime>() {
        mime
    } else {
        return false;
    };

    let is_json_content_type = mime.type_() == "application"
        && (mime.subtype() == "json" || mime.suffix().map_or(false, |name| name == "json"));

    is_json_content_type
}
