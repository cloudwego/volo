//! Json utilities of Volo-HTTP
use serde::{de::DeserializeOwned, ser::Serialize};
pub use sonic_rs::Error;

#[cfg(feature = "server")]
mod server;

pub(crate) fn serialize<T>(data: &T) -> Result<Vec<u8>, Error>
where
    T: Serialize,
{
    sonic_rs::to_vec(data)
}

#[allow(dead_code)]
pub(crate) fn serialize_to_writer<W, T>(writer: W, data: &T) -> Result<(), Error>
where
    W: sonic_rs::writer::WriteExt,
    T: Serialize,
{
    sonic_rs::to_writer(writer, data)
}

pub(crate) fn deserialize<T>(data: &[u8]) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    sonic_rs::from_slice(data)
}

/// A wrapper type with [`FromRequest`](crate::server::extract::FromRequest) and
/// [`IntoResponse`](crate::server::response::IntoResponse)
///
/// The [`Json`] can be parameter or response of a handler.
///
/// # Examples
///
/// Use [`Json`] as parameter:
///
/// ```
/// use serde::Deserialize;
/// use volo_http::{
///     json::Json,
///     server::route::{post, Router},
/// };
///
/// #[derive(Debug, Deserialize)]
/// struct User {
///     username: String,
///     password: String,
/// }
///
/// async fn login(Json(user): Json<User>) {
///     println!("user: {user:?}");
/// }
///
/// let router: Router = Router::new().route("/api/v2/login", post(login));
/// ```
///
/// User [`Json`] as response:
///
/// ```
/// use serde::Serialize;
/// use volo_http::{
///     json::Json,
///     server::route::{get, Router},
/// };
///
/// #[derive(Debug, Serialize)]
/// struct User {
///     username: String,
///     password: String,
/// }
///
/// async fn user_info() -> Json<User> {
///     let user = User {
///         username: String::from("admin"),
///         password: String::from("passw0rd"),
///     };
///     Json(user)
/// }
///
/// let router: Router = Router::new().route("/api/v2/info", get(user_info));
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct Json<T>(pub T);
