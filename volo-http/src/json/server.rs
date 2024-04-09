use bytes::Bytes;
use http::{
    header::{self, HeaderMap},
    request::Parts,
    StatusCode,
};
use hyper::body::Incoming;
use mime::Mime;
use serde::de::DeserializeOwned;

use super::{deserialize, Error, Json};
use crate::{
    context::ServerContext,
    error::server::{invalid_content_type, ExtractBodyError},
    response::ServerResponse,
    server::{extract::FromRequest, IntoResponse},
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
    type Rejection = ExtractBodyError;

    async fn from_request(
        cx: &mut ServerContext,
        parts: Parts,
        body: Incoming,
    ) -> Result<Self, Self::Rejection> {
        if !json_content_type(&parts.headers) {
            return Err(invalid_content_type());
        }

        let bytes = Bytes::from_request(cx, parts, body).await?;
        let json = deserialize(&bytes).map_err(ExtractBodyError::Json)?;

        Ok(Json(json))
    }
}

fn json_content_type(headers: &HeaderMap) -> bool {
    let content_type = match headers.get(header::CONTENT_TYPE) {
        Some(content_type) => content_type,
        None => {
            return false;
        }
    };

    let content_type = match content_type.to_str() {
        Ok(s) => s,
        Err(_) => {
            return false;
        }
    };

    let mime_type = match content_type.parse::<Mime>() {
        Ok(mime_type) => mime_type,
        Err(_) => {
            return false;
        }
    };

    // `application/json` or `application/json+foo`
    if mime_type.type_() == mime::APPLICATION && mime_type.subtype() == mime::JSON {
        return true;
    }

    // `application/foo+json`
    if mime_type.suffix() == Some(mime::JSON) {
        return true;
    }

    false
}
