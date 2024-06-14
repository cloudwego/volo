use http::{
    header::{self, HeaderValue},
    status::StatusCode,
};

use super::IntoResponse;
use crate::{body::Body, response::ServerResponse};

pub struct Redirect {
    status: StatusCode,
    location: HeaderValue,
}

impl Redirect {
    /// Create a new [`Redirect`] with a status code and a target location.
    ///
    /// # Panics
    ///
    /// If the location is not a valid header value.
    pub fn with_status_code(status: StatusCode, location: &str) -> Self {
        debug_assert!(status.is_redirection());

        Self {
            status,
            location: HeaderValue::from_str(location)
                .expect("The target location is not a valid header value"),
        }
    }

    /// Create a new [`Redirect`] with [`301 Moved Permanently`][301] status code.
    ///
    /// # Panics
    ///
    /// If the location is not a valid header value.
    ///
    /// [301]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/301
    pub fn moved_permanently(location: &str) -> Self {
        Self::with_status_code(StatusCode::MOVED_PERMANENTLY, location)
    }

    /// Create a new [`Redirect`] with [`302 Found`][302] status code.
    ///
    /// # Panics
    ///
    /// If the location is not a valid header value.
    ///
    /// [302]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/302
    pub fn found(location: &str) -> Self {
        Self::with_status_code(StatusCode::FOUND, location)
    }

    /// Create a new [`Redirect`] with [`303 Found`][303] status code.
    ///
    /// # Panics
    ///
    /// If the location is not a valid header value.
    ///
    /// [303]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/303
    pub fn see_other(location: &str) -> Self {
        Self::with_status_code(StatusCode::SEE_OTHER, location)
    }

    /// Create a new [`Redirect`] with [`307 Temporary Redirect`][307] status code.
    ///
    /// # Panics
    ///
    /// If the location is not a valid header value.
    ///
    /// [307]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/307
    pub fn temporary_redirect(location: &str) -> Self {
        Self::with_status_code(StatusCode::TEMPORARY_REDIRECT, location)
    }

    /// Create a new [`Redirect`] with [`308 Permanent Redirect`][308] status code.
    ///
    /// # Panics
    ///
    /// If the location is not a valid header value.
    ///
    /// [308]: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/308
    pub fn permanent_redirect(location: &str) -> Self {
        Self::with_status_code(StatusCode::PERMANENT_REDIRECT, location)
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> ServerResponse {
        ServerResponse::builder()
            .status(self.status)
            .header(header::LOCATION, self.location)
            .body(Body::default())
            .expect("infallible")
    }
}
