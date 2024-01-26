use bytes::Bytes;
use hyper::{
    body::Incoming,
    http::{
        header::{self, HeaderMap},
        StatusCode,
    },
};
use serde::{de::DeserializeOwned, ser::Serialize};
#[cfg(all(feature = "serde_json", feature = "sonic_json"))]
compile_error!("features `serde_json` and `sonic_json` cannot be enabled at the same time.");
#[cfg(feature = "serde_json")]
pub use serde_json::Error;
#[cfg(feature = "sonic_json")]
pub use sonic_rs::Error;

use crate::{
    extract::{FromRequest, RejectionError},
    response::IntoResponse,
    Response, ServerContext,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct Json<T>(pub T);

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        #[cfg(feature = "sonic_json")]
        let ser = sonic_rs::to_string(&self.0);
        #[cfg(feature = "serde_json")]
        let ser = serde_json::to_string(&self.0);

        match ser {
            Ok(s) => s.into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Sync,
{
    type Rejection = RejectionError;

    async fn from_request(
        cx: &mut ServerContext,
        body: Incoming,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        if !json_content_type(cx.headers()) {
            return Err(RejectionError::InvalidContentType);
        }

        let bytes = Bytes::from_request(cx, body, state).await?;
        #[cfg(feature = "sonic_json")]
        let json = sonic_rs::from_slice(&bytes).map_err(RejectionError::JsonRejection)?;
        #[cfg(feature = "serde_json")]
        let json =
            serde_json::from_slice::<T>(bytes.as_ref()).map_err(RejectionError::JsonRejection)?;

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
