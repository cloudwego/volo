//! HTTP request and response statistics shared across client and server contexts.

use chrono::{DateTime, Local};
use http::{method::Method, status::StatusCode, uri::Uri};

/// Shared HTTP statistics captured for every request on both client and server sides
#[derive(Debug, Default, Clone)]
pub struct CommonStats {
    /// The time at which request processing began
    pub process_start_time: DateTime<Local>,

    /// The time at which request processing completed
    pub process_end_time: DateTime<Local>,

    /// The HTTP method of the request (e.g. `GET`, `POST`)
    pub method: Method,

    /// The full URI of the request
    pub uri: Uri,

    /// The HTTP status code of the response.
    ///
    /// Status code may be None if the service failed
    pub status_code: Option<StatusCode>,

    /// Size of the request body in bytes
    pub req_size: i64,

    /// Size of the response body in bytes
    pub resp_size: i64,

    /// Whether the request resulted in an error
    pub is_error: bool,
}
