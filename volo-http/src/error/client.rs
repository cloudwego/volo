//! Generic error types for client

use std::{error::Error, fmt, net::SocketAddr};

use http::uri::Uri;
use paste::paste;
use volo::{context::Endpoint, net::Address};

use super::BoxError;
use crate::body::BodyConvertError;

/// [`Result`](std::result::Result) with [`ClientError`] as its error by default.
pub type Result<T, E = ClientError> = std::result::Result<T, E>;

macro_rules! tri {
    ($result:expr) => {
        match $result {
            Ok(val) => val,
            Err(err) => return Err(err),
        }
    };
    ($result:expr, $($arg:tt)*) => {
        match $result {
            Ok(val) => val,
            Err(err) => {
                ::tracing::info!($($arg)*);
                return Err(err);
            }
        }
    };
}
pub(crate) use tri;

/// Generic client error
#[derive(Debug)]
pub struct ClientError {
    kind: ErrorKind,
    source: Option<BoxError>,
    // The type `Uri` takes up 88 bytes, which causes the entire `ClientError` to take up 144 bytes
    // and clippy will throw `clippy::result_large_err`.
    //
    // Considering that the field will not be used in the hot path, in order to avoid the problem
    // of `ClientError` being too large, we added `Box` to `Uri`.
    uri: Option<Box<Uri>>,
    addr: Option<SocketAddr>,
}

impl ClientError {
    /// Create a new [`ClientError`] using the given [`ErrorKind`] and [`Error`]
    pub fn new<E>(kind: ErrorKind, error: Option<E>) -> Self
    where
        E: Into<BoxError>,
    {
        Self {
            kind,
            source: error.map(Into::into),
            uri: None,
            addr: None,
        }
    }

    /// Set a [`Uri`] to the [`ClientError`].
    #[inline]
    pub fn set_url(&mut self, uri: Uri) {
        self.uri = Some(Box::new(uri));
    }

    /// Set a [`SocketAddr`] to the [`ClientError`].
    #[inline]
    pub fn set_addr(&mut self, addr: SocketAddr) {
        self.addr = Some(addr);
    }

    /// Consume current [`ClientError`] and return a new one with given [`Uri`].
    #[inline]
    pub fn with_url(mut self, uri: Uri) -> Self {
        self.uri = Some(Box::new(uri));
        self
    }

    /// Remove [`Uri`] from the [`ClientError`].
    #[inline]
    pub fn without_url(mut self) -> Self {
        self.uri = None;
        self
    }

    /// Consume current [`ClientError`] and return a new one with given [`SocketAddr`].
    #[inline]
    pub fn with_addr(mut self, addr: SocketAddr) -> Self {
        self.addr = Some(addr);
        self
    }

    /// Consume current [`ClientError`] and return a new one with [`SocketAddr`] from the
    /// [`Address`] if exists.
    #[inline]
    pub fn with_address(mut self, address: Address) -> Self {
        match address {
            Address::Ip(addr) => self.addr = Some(addr),
            #[cfg(target_family = "unix")]
            Address::Unix(_) => {}
        }
        self
    }

    /// Consume current [`ClientError`] and return a new one with [`SocketAddr`] from the
    /// [`Address`] if exists.
    #[inline]
    pub fn with_endpoint(mut self, ep: &Endpoint) -> Self {
        if let Some(Address::Ip(addr)) = &ep.address {
            self.addr = Some(*addr);
        }
        self
    }

    /// Get a reference to the [`ErrorKind`]
    #[inline]
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// Get a reference to the [`Uri`] if it exists
    #[inline]
    pub fn uri(&self) -> Option<&Uri> {
        self.uri.as_deref()
    }

    /// Get a reference to the [`SocketAddr`] if it exists
    #[inline]
    pub fn addr(&self) -> Option<&SocketAddr> {
        self.addr.as_ref()
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(addr) = &self.addr {
            write!(f, " to addr {addr}")?;
        }
        if let Some(uri) = &self.uri {
            write!(f, " for uri `{uri}`")?;
        }
        if let Some(source) = &self.source {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}

impl Error for ClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref()?.as_ref())
    }
}

/// Error kind of [`ClientError`]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Error occurs when building a client or a request
    Builder,
    /// Something wrong with the [`ClientContext`](crate::context::ClientContext)
    Context,
    /// Failed to connect to target server
    Connect,
    /// Fails to send a request to target server
    Request,
    /// Something wrong from [`LoadBalance`][LoadBalance] or [`Discover`][Discover]
    ///
    /// [LoadBalance]: volo::loadbalance::LoadBalance
    /// [Discover]: volo::discovery::Discover
    LoadBalance,
    /// Something wrong when processing on [`Body`](crate::body::Body)
    Body,
    /// Other unrecognizable errors
    Other,
}

/// Create a [`ClientError`] with [`ErrorKind::Builder`]
pub fn builder_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Builder, Some(error))
}

/// Create a [`ClientError`] with [`ErrorKind::Context`]
pub fn connect_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Connect, Some(error))
}

/// Create a [`ClientError`] with [`ErrorKind::Context`]
pub fn context_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Context, Some(error))
}

/// Create a [`ClientError`] with [`ErrorKind::Request`]
pub fn request_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Request, Some(error))
}

/// Create a [`ClientError`] with [`ErrorKind::LoadBalance`]
pub fn lb_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::LoadBalance, Some(error))
}

/// Create a [`ClientError`] with [`ErrorKind::Body`]
pub fn body_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Body, Some(error))
}

/// Create a [`ClientError`] with [`ErrorKind::Other`]
pub fn other_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Other, Some(error))
}

impl From<BodyConvertError> for ClientError {
    fn from(value: BodyConvertError) -> Self {
        ClientError::new(ErrorKind::Body, Some(BoxError::from(value)))
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builder => f.write_str("builder error"),
            Self::Context => f.write_str("processing context error"),
            Self::Connect => f.write_str("connect error"),
            Self::Request => f.write_str("sending request error"),
            Self::LoadBalance => f.write_str("load balance error"),
            Self::Body => f.write_str("processing body error"),
            Self::Other => f.write_str("error"),
        }
    }
}

macro_rules! simple_error {
    ($(#[$attr:meta])* $kind:ident => $name:ident => $msg:literal) => {
        paste! {
            #[doc = $kind " error \"" $msg "\""]
            $(#[$attr])*
            #[derive(Debug, PartialEq, Eq)]
            pub struct $name;

            $(#[$attr])*
            impl ::std::fmt::Display for $name {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    f.write_str($msg)
                }
            }

            $(#[$attr])*
            impl ::std::error::Error for $name {}

            $(#[$attr])*
            pub(crate) fn [<$name:snake>]() -> ClientError {
                ClientError::new(ErrorKind::$kind, Some($name))
            }
        }
    };

    ($(#[$attr:meta])* $kind:ident => $name:ident($inner:ty) => $msg:literal) => {
        paste! {
            #[doc = $kind " error \"" $msg "\""]
            $(#[$attr])*
            #[derive(Debug, PartialEq, Eq)]
            pub struct $name($inner);

            $(#[$attr])*
            impl ::std::fmt::Display for $name {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    write!(f, concat!($msg, ": {}"), self.0)
                }
            }

            $(#[$attr])*
            impl ::std::error::Error for $name {}

            $(#[$attr])*
            pub(crate) fn [<$name:snake>](inner: $inner) -> ClientError {
                ClientError::new(ErrorKind::$kind, Some($name(inner)))
            }
        }
    };
}

simple_error!(Builder => NoAddress => "missing target address");
simple_error!(Builder => BadScheme(::http::uri::Scheme) => "bad scheme");
#[cfg(not(all(feature = "http1", feature = "http2")))]
simple_error!(Builder => BadVersion => "bad http protocol version");
simple_error!(Builder => BadHostName(::faststr::FastStr) => "bad host name");
simple_error!(Builder => SchemeUnavailable => "scheme is unavailable in current target");
simple_error!(Builder => PortUnavailable => "port is unavailable in current target");
simple_error!(Connect => Retry => "retry");
simple_error!(Request => Timeout => "request timeout");
simple_error!(LoadBalance => NoAvailableEndpoint => "no available endpoint");

#[cfg(test)]
mod client_error_tests {
    use std::error::Error;

    use crate::error::client::{
        BadHostName, BadScheme, NoAddress, NoAvailableEndpoint, Timeout, bad_host_name, bad_scheme,
        no_address, no_available_endpoint, timeout,
    };

    #[test]
    fn types_downcast() {
        assert!(no_address().source().unwrap().is::<NoAddress>());
        assert!(
            bad_scheme(::http::uri::Scheme::HTTP)
                .source()
                .unwrap()
                .is::<BadScheme>()
        );
        assert!(
            bad_host_name(::faststr::FastStr::from_static_str("foo"))
                .source()
                .unwrap()
                .is::<BadHostName>()
        );
        assert!(timeout().source().unwrap().is::<Timeout>());
        assert!(
            no_available_endpoint()
                .source()
                .unwrap()
                .is::<NoAvailableEndpoint>()
        );
    }
}
