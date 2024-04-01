use std::{error::Error, fmt};

use http::{StatusCode, Uri};
use paste::paste;

use super::BoxError;

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Debug)]
pub struct ClientError {
    kind: ErrorKind,
    source: Option<BoxError>,
    url: Option<Uri>,
}

impl ClientError {
    pub(crate) fn new<E>(kind: ErrorKind, error: Option<E>) -> Self
    where
        E: Into<BoxError>,
    {
        Self {
            kind,
            source: error.map(Into::into),
            url: None,
        }
    }

    pub fn with_url(self, url: Uri) -> Self {
        Self {
            kind: self.kind,
            source: self.source,
            url: Some(url),
        }
    }

    pub fn without_url(self) -> Self {
        Self {
            kind: self.kind,
            source: self.source,
            url: None,
        }
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    pub fn url(&self) -> Option<&Uri> {
        self.url.as_ref()
    }

    pub fn url_mut(&mut self) -> Option<&mut Uri> {
        self.url.as_mut()
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(ref url) = self.url {
            write!(f, "for url `{url}`")?;
        }
        if let Some(ref source) = self.source {
            write!(f, ": {source}")?;
        }
        Ok(())
    }
}

impl Error for ClientError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Builder,
    Context,
    Request,
    Status(StatusCode),
    Body,
}

pub fn builder_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Builder, Some(error))
}

pub fn context_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Context, Some(error))
}

pub fn request_error<E>(error: E) -> ClientError
where
    E: Into<BoxError>,
{
    ClientError::new(ErrorKind::Request, Some(error))
}

pub fn status_error(status: StatusCode) -> ClientError {
    ClientError::new(ErrorKind::Status(status), None::<ClientError>)
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builder => f.write_str("builder error"),
            Self::Context => f.write_str("processing context error"),
            Self::Request => f.write_str("sending request error"),
            Self::Status(ref status) => {
                let prefix = if status.is_client_error() {
                    "HTTP status client error"
                } else {
                    "HTTP status server error"
                };
                write!(f, "{prefix} ({status})")
            }
            Self::Body => f.write_str("processing body error"),
        }
    }
}

macro_rules! simple_error {
    ($(#[$attr:meta])* $kind:ident => $name:ident => $msg:literal) => {
        $(#[$attr])*
        #[derive(Debug)]
        pub struct $name;

        $(#[$attr])*
        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str($msg)
            }
        }

        $(#[$attr])*
        impl ::std::error::Error for $name {}

        paste! {
            $(#[$attr])*
            pub(crate) fn [<$name:snake>]() -> ClientError {
                ClientError::new(ErrorKind::$kind, Some($name))
            }
        }
    };
}

macro_rules! simple_error_with_url {
    ($(#[$attr:meta])* $kind:ident => $name:ident => $msg:literal) => {
        $(#[$attr])*
        #[derive(Debug)]
        pub struct $name;

        $(#[$attr])*
        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str($msg)
            }
        }

        $(#[$attr])*
        impl ::std::error::Error for $name {}

        paste! {
            $(#[$attr])*
            pub(crate) fn [<$name:snake>](url: Uri) -> ClientError {
                ClientError::new(ErrorKind::$kind, Some($name))
                    .with_url(url)
            }
        }
    };
}

simple_error!(Builder => NoUri => "uri not found");
simple_error_with_url!(Builder => UriWithoutHost => "host not found in uri");
simple_error_with_url!(Builder => BadScheme => "bad scheme");
simple_error_with_url!(Builder => BadHostName => "bad host name");
simple_error!(Request => NoAddress => "missing target address");
