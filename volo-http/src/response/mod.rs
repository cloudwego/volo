mod into_response;

use hyper::body::Incoming;

pub use self::into_response::IntoResponse;
use crate::body::Body;

pub type Response<B = Body> = http::Response<B>;

pub type ServerResponse<B = Body> = http::Response<B>;
pub type ClientResponse<B = Incoming> = http::Response<B>;
