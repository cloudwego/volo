use std::{error::Error, fmt};

use paste::paste;

use super::BoxError;

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Debug)]
pub struct ClientError {
    kind: Kind,
    error: ClientErrorInner,
}

impl ClientError {
    pub fn new(kind: Kind, error: ClientErrorInner) -> Self {
        Self { kind, error }
    }

    pub fn new_other<E>(kind: Kind, error: E) -> Self
    where
        E: Into<BoxError>,
    {
        Self::new(kind, ClientErrorInner::Other(error.into()))
    }

    pub fn kind(&self) -> &Kind {
        &self.kind
    }

    pub fn inner(&self) -> &ClientErrorInner {
        &self.error
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.error)
    }
}

impl Error for ClientError {}

macro_rules! error_kind {
    ($($name:ident => $msg:literal,)+) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum Kind {
            $($name,)+
        }

        paste! {
            $(
                pub(crate) fn [<$name:lower _error>]<E>(error: E) -> ClientError
                where
                    E: Into<BoxError>,
                {
                    ClientError::new_other(Kind::$name, error)
                }
            )+
        }

        impl std::fmt::Display for Kind {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(
                        Self::$name => f.write_str($msg),
                    )+
                }
            }
        }
    };
}

macro_rules! client_error_inner {
    ($($(#[$attr:meta])* $kind:ident => $name:ident => $msg:literal,)+) => {
        #[derive(Debug)]
        pub enum ClientErrorInner {
            $($(#[$attr])* $name,)+
            Other(BoxError),
        }

        impl std::fmt::Display for ClientErrorInner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(
                        $(#[$attr])*
                        Self::$name => f.write_str($msg),
                    )+
                    Self::Other(err) => write!(f, "{}", err),
                }
            }
        }

        impl std::error::Error for ClientErrorInner {}

        paste! {
            $(
                $(#[$attr])*
                pub(crate) fn [<$name:snake>]() -> ClientError {
                    ClientError::new(Kind::$kind, ClientErrorInner::$name)
                }
            )+
        }
    };
}

error_kind! {
    Builder => "build error",
    Request => "sending request error",
}

client_error_inner! {
    Builder => NoUri => "uri not found",
    Builder => UriWithoutHost => "host not found in uri",
    Builder => BadScheme => "bad scheme",
    Builder => BadHostName => "bad host name",
    Request => NoAddress => "missing target address",
}
