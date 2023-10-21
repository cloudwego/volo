use std::fmt::{self, Display, Formatter};

use pilota::thrift::{
    DecodeError, EncodeError, Error as PilotaError, Message, ProtocolError, TAsyncInputProtocol,
    TInputProtocol, TLengthProtocol, TOutputProtocol, TStructIdentifier, TType, TransportError,
};
use volo::loadbalance::error::{LoadBalanceError, Retryable};

use crate::AnyhowError;

pub type Result<T, E = Error> = core::result::Result<T, E>;

const TAPPLICATION_EXCEPTION: TStructIdentifier = TStructIdentifier {
    name: "TApplicationException",
};

#[derive(Debug)]
pub enum Error {
    Transport(pilota::thrift::TransportError),
    Protocol(pilota::thrift::ProtocolError),

    /// Errors encountered within auto-generated code, or when incoming
    /// or outgoing messages violate the Thrift spec.
    ///
    /// These include *out-of-order messages* and *missing required struct
    /// fields*.
    ///
    /// This variant also functions as a catch-all: errors from handler
    /// functions are automatically returned as an `ApplicationError`.
    Application(ApplicationError),
}

#[derive(Debug, Clone, Copy)]
pub struct DummyError;

impl Message for DummyError {
    fn encode<T: TOutputProtocol>(&self, _protocol: &mut T) -> Result<(), EncodeError> {
        panic!()
    }

    fn decode<T: TInputProtocol>(_protocol: &mut T) -> Result<Self, DecodeError> {
        panic!()
    }

    async fn decode_async<T: TAsyncInputProtocol>(_protocol: &mut T) -> Result<Self, DecodeError> {
        panic!()
    }

    fn size<T: TLengthProtocol>(&self, _protocol: &mut T) -> usize {
        panic!()
    }
}

impl Error {
    pub fn append_msg(&mut self, msg: &str) {
        match self {
            Error::Transport(e) => {
                e.message.push_str(msg);
            }
            Error::Protocol(e) => {
                e.message.push_str(msg);
            }
            Error::Application(e) => e.message.push_str(msg),
        }
    }
}

impl From<TransportError> for Error {
    fn from(value: TransportError) -> Self {
        Error::Transport(value)
    }
}

impl From<PilotaError> for Error {
    fn from(e: PilotaError) -> Self {
        match e {
            PilotaError::Transport(e) => Error::Transport(e),
            PilotaError::Protocol(e) => Error::Protocol(e),
        }
    }
}

impl From<EncodeError> for Error {
    fn from(value: EncodeError) -> Self {
        Error::Protocol(ProtocolError {
            kind: value.kind,
            message: value.to_string(),
        })
    }
}

impl From<DecodeError> for Error {
    fn from(value: DecodeError) -> Self {
        macro_rules! protocol_err {
            ($kind:ident) => {
                Error::Protocol(ProtocolError {
                    kind: pilota::thrift::ProtocolErrorKind::$kind,
                    message: value.message,
                })
            };
        }

        match value.kind {
            pilota::thrift::DecodeErrorKind::InvalidData => protocol_err!(InvalidData),
            pilota::thrift::DecodeErrorKind::NegativeSize => protocol_err!(NegativeSize),
            pilota::thrift::DecodeErrorKind::BadVersion => protocol_err!(BadVersion),
            pilota::thrift::DecodeErrorKind::NotImplemented => protocol_err!(NotImplemented),
            pilota::thrift::DecodeErrorKind::DepthLimit => protocol_err!(DepthLimit),

            pilota::thrift::DecodeErrorKind::UnknownMethod => {
                Error::Application(ApplicationError {
                    kind: ApplicationErrorKind::UNKNOWN_METHOD,
                    message: value.message,
                })
            }
            pilota::thrift::DecodeErrorKind::IOError(e) => {
                Error::Transport(TransportError::from(e))
            }
            pilota::thrift::DecodeErrorKind::WithContext(_) => Error::Protocol(ProtocolError::new(
                pilota::thrift::ProtocolErrorKind::Unknown,
                value.to_string(),
            )),
            pilota::thrift::DecodeErrorKind::Unknown => protocol_err!(Unknown),
        }
    }
}

impl From<ApplicationError> for Error {
    fn from(e: ApplicationError) -> Self {
        Error::Application(e)
    }
}

impl From<LoadBalanceError> for Error {
    fn from(err: LoadBalanceError) -> Self {
        new_application_error(ApplicationErrorKind::INTERNAL_ERROR, err.to_string())
    }
}

impl Retryable for Error {
    fn retryable(&self) -> bool {
        if let Error::Transport(_) = self {
            return true;
        }
        false
    }
}

impl From<AnyhowError> for Error {
    fn from(err: AnyhowError) -> Self {
        new_application_error(ApplicationErrorKind::UNKNOWN, err.to_string())
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for Error {
    fn from(err: Box<dyn std::error::Error + Send + Sync>) -> Self {
        new_application_error(ApplicationErrorKind::UNKNOWN, err.to_string())
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// Information about errors in auto-generated code or in user-implemented
/// service handlers.
#[derive(Debug, Eq, PartialEq)]
pub struct ApplicationError {
    /// Application error variant.
    ///
    /// If a specific `ApplicationErrorKind` does not apply use
    /// `ApplicationErrorKind::Unknown`.
    pub kind: ApplicationErrorKind,
    /// Human-readable error message.
    pub message: String,
}

impl ApplicationError {
    /// Create a new `ApplicationError`.
    pub fn new<S: Into<String>>(kind: ApplicationErrorKind, message: S) -> ApplicationError {
        ApplicationError {
            kind,
            message: message.into(),
        }
    }
}

impl Display for ApplicationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let error_text = match self.kind {
            ApplicationErrorKind::UNKNOWN => "service error",
            ApplicationErrorKind::UNKNOWN_METHOD => "unknown service method",
            ApplicationErrorKind::INVALID_MESSAGE_TYPE => "wrong message type received",
            ApplicationErrorKind::WRONG_METHOD_NAME => "unknown method reply received",
            ApplicationErrorKind::BAD_SEQUENCE_ID => "out of order sequence id",
            ApplicationErrorKind::MISSING_RESULT => "missing method result",
            ApplicationErrorKind::INTERNAL_ERROR => "remote service threw exception",
            ApplicationErrorKind::PROTOCOL_ERROR => "protocol error",
            ApplicationErrorKind::INVALID_TRANSFORM => "invalid transform",
            ApplicationErrorKind::INVALID_PROTOCOL => "invalid protocol requested",
            ApplicationErrorKind::UNSUPPORTED_CLIENT_TYPE => "unsupported protocol client",
            _ => "other error",
        };

        write!(f, "{}, msg: {}", error_text, self.message)
    }
}

impl Message for ApplicationError {
    /// Convert an `ApplicationError` into its wire representation and write
    /// it to the remote.
    ///
    /// Application code **should never** call this method directly.
    fn encode<T: TOutputProtocol>(&self, protocol: &mut T) -> Result<(), EncodeError> {
        protocol.write_struct_begin(&TAPPLICATION_EXCEPTION)?;

        protocol.write_field_begin(TType::Binary, 1)?;
        protocol.write_string(&self.message)?;
        protocol.write_field_end()?;

        protocol.write_field_begin(TType::I32, 2)?;
        protocol.write_i32(self.kind.as_i32())?;
        protocol.write_field_end()?;

        protocol.write_field_stop()?;
        protocol.write_struct_end()?;

        protocol.flush()?;
        Ok(())
    }

    fn decode<T: TInputProtocol>(protocol: &mut T) -> Result<Self, DecodeError> {
        let mut message = "general remote error".to_owned();
        let mut kind = ApplicationErrorKind::UNKNOWN;

        protocol.read_struct_begin()?;

        loop {
            let field_ident = protocol.read_field_begin()?;

            if field_ident.field_type == TType::Stop {
                break;
            }

            let id = field_ident
                .id
                .expect("sender should always specify id for non-STOP field");

            match id {
                1 => {
                    let remote_message = protocol.read_string()?;
                    protocol.read_field_end()?;
                    message = (&*remote_message).into();
                }
                2 => {
                    let remote_type_as_int = protocol.read_i32()?;
                    let remote_kind: ApplicationErrorKind = TryFrom::try_from(remote_type_as_int)
                        .unwrap_or(ApplicationErrorKind::UNKNOWN);
                    protocol.read_field_end()?;
                    kind = remote_kind;
                }
                _ => {
                    protocol.skip(field_ident.field_type)?;
                }
            }
        }

        protocol.read_struct_end()?;

        Ok(ApplicationError { kind, message })
    }

    async fn decode_async<T: TAsyncInputProtocol>(protocol: &mut T) -> Result<Self, DecodeError> {
        let mut message = "general remote error".to_owned();
        let mut kind = ApplicationErrorKind::UNKNOWN;

        protocol.read_struct_begin().await?;

        loop {
            let field_ident = protocol.read_field_begin().await?;

            if field_ident.field_type == TType::Stop {
                break;
            }

            let id = field_ident
                .id
                .expect("sender should always specify id for non-STOP field");

            match id {
                1 => {
                    let remote_message = protocol.read_string().await?;
                    protocol.read_field_end().await?;
                    message = (&*remote_message).into();
                }
                2 => {
                    let remote_type_as_int = protocol.read_i32().await?;
                    let remote_kind: ApplicationErrorKind = TryFrom::try_from(remote_type_as_int)
                        .unwrap_or(ApplicationErrorKind::UNKNOWN);
                    protocol.read_field_end().await?;
                    kind = remote_kind;
                }
                _ => {
                    protocol.skip(field_ident.field_type).await?;
                }
            }
        }

        protocol.read_struct_end().await?;

        Ok(ApplicationError { kind, message })
    }

    fn size<T: TLengthProtocol>(&self, protocol: &mut T) -> usize {
        protocol.struct_begin_len(&TAPPLICATION_EXCEPTION)
            + protocol.field_begin_len(TType::Binary, Some(1))
            + protocol.string_len(&self.message)
            + protocol.field_end_len()
            + protocol.field_begin_len(TType::I32, Some(2))
            + protocol.i32_len(self.kind.as_i32())
            + protocol.field_end_len()
            + protocol.field_stop_len()
            + protocol.struct_end_len()
    }
}

/// Auto-generated or user-implemented code error categories.
///
/// This list may grow, and it is not recommended to match against it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ApplicationErrorKind(i32);

impl ApplicationErrorKind {
    /// Catch-all application error.
    pub const UNKNOWN: Self = Self(0);
    /// Made service call to an unknown service method.
    pub const UNKNOWN_METHOD: Self = Self(1);
    /// Received an unknown Thrift message type. That is, not one of the
    /// `thrift::protocol::TMessageType` variants.
    pub const INVALID_MESSAGE_TYPE: Self = Self(2);
    /// Method name in a service reply does not match the name of the
    /// receiving service method.
    pub const WRONG_METHOD_NAME: Self = Self(3);
    /// Received an out-of-order Thrift message.
    pub const BAD_SEQUENCE_ID: Self = Self(4);
    /// Service reply is missing required fields.
    pub const MISSING_RESULT: Self = Self(5);
    /// Auto-generated code failed unexpectedly.
    pub const INTERNAL_ERROR: Self = Self(6);
    /// Thrift protocol error. When possible use `Error::ProtocolError` with a
    /// specific `ProtocolErrorKind` instead.
    pub const PROTOCOL_ERROR: Self = Self(7);
    /// *Unknown*. Included only for compatibility with existing Thrift
    /// implementations.
    pub const INVALID_TRANSFORM: Self = Self(8); // ??
    /// Thrift endpoint requested, or is using, an unsupported encoding.
    pub const INVALID_PROTOCOL: Self = Self(9); // ??
    /// Thrift endpoint requested, or is using, an unsupported auto-generated
    /// client type.
    pub const UNSUPPORTED_CLIENT_TYPE: Self = Self(10); // ??

    pub fn as_i32(self) -> i32 {
        self.0
    }
}

impl From<i32> for ApplicationErrorKind {
    fn from(from: i32) -> Self {
        Self(from)
    }
}

impl From<ApplicationErrorKind> for i32 {
    fn from(value: ApplicationErrorKind) -> Self {
        value.as_i32()
    }
}

/// Create a new `Error` instance of type `Application` that wraps an
/// `ApplicationError`.
pub fn new_application_error<S: Into<String>>(kind: ApplicationErrorKind, message: S) -> Error {
    Error::Application(ApplicationError::new(kind, message))
}

#[derive(Debug, thiserror::Error)]
pub enum UserError<T> {
    #[error("a exception from remote: {0}")]
    UserException(T),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ResponseError<T> {
    #[error("a exception from remote: {0}")]
    UserException(T),
    #[error("application error: {0}")]
    Application(ApplicationError),
    #[error("transport error: {0}")]
    Transport(TransportError),
    #[error("protocol error: {0}")]
    Protocol(ProtocolError),
}

impl<T> From<Error> for ResponseError<T> {
    fn from(e: Error) -> Self {
        match e {
            Error::Transport(e) => ResponseError::Transport(e),
            Error::Protocol(e) => ResponseError::Protocol(e),
            Error::Application(e) => ResponseError::Application(e),
        }
    }
}
