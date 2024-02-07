use bytes::Bytes;
use http::{
    header::{self, HeaderMap},
    request::Parts,
    StatusCode,
};
use hyper::body::Incoming;
use serde::{de::DeserializeOwned, ser::Serialize};
#[cfg(all(feature = "serde_json", feature = "sonic_json"))]
compile_error!("features `serde_json` and `sonic_json` cannot be enabled at the same time.");
#[cfg(feature = "serde_json")]
pub use serde_json::Error;
#[cfg(feature = "sonic_json")]
pub use sonic_rs::Error;

use crate::{
    body::Body,
    context::ServerContext,
    extract::{FromRequest, RejectionError},
    response::{IntoResponse, ServerResponse},
};

pub(crate) fn serialize<T>(data: &T) -> Result<Vec<u8>, Error>
where
    T: Serialize,
{
    #[cfg(feature = "sonic_json")]
    let res = sonic_rs::to_vec(data);

    #[cfg(feature = "serde_json")]
    let res = serde_json::to_vec(data);

    res
}

pub(crate) fn deserialize<T>(data: &[u8]) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    #[cfg(feature = "sonic_json")]
    let res = sonic_rs::from_slice(data);

    #[cfg(feature = "serde_json")]
    let res = serde_json::from_slice(data);

    res
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Json<T>(pub T);

impl<T> TryFrom<Json<T>> for Body
where
    T: Serialize,
{
    type Error = Error;

    fn try_from(value: Json<T>) -> Result<Self, Self::Error> {
        serialize(&value.0).map(Body::from)
    }
}

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
