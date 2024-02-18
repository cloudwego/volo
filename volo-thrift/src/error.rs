use std::{
    fmt::{self, Display, Formatter},
    io,
};

use pilota::{AHashMap, FastStr};

pub use pilota::thrift::{
    new_application_exception, new_protocol_exception, ApplicationException,
    ApplicationExceptionKind, ProtocolException, ProtocolExceptionKind, ThriftException,
    TransportException,
};
use volo::loadbalance::error::{LoadBalanceError, Retryable};

// pub type Result<T, E = Error> = core::result::Result<T, E>;

#[derive(Debug)]
pub enum ServerError {
    // #[error("application exception: {0}")]
    Application(ApplicationException),
    // #[error("biz error: {0}")]
    Biz(BizError),
}

impl<E> From<E> for ServerError
where
    E: std::error::Error + Send + Sync + 'static,
{
    #[cold]
    fn from(error: E) -> Self {
        // First convert `E` to a boxed trait object so we can attempt downcasting.
        let error_boxed = Box::new(error) as Box<dyn std::error::Error + Send + Sync>;

        // Use if let to try downcasting to ApplicationException.
        match error_boxed.downcast::<ApplicationException>() {
            Ok(application_error) => ServerError::Application(*application_error),
            Err(e) => match e.downcast::<BizError>() {
                Ok(biz_error) => ServerError::Biz(*biz_error),
                Err(e) => ServerError::Application(ApplicationException::new(
                    ApplicationExceptionKind::INTERNAL_ERROR,
                    e.to_string(),
                )),
            },
        }
    }
}

impl Display for ServerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::Application(e) => write!(f, "application exception: {}", e),
            ServerError::Biz(e) => write!(f, "biz error: {}", e),
        }
    }
}

impl ServerError {
    pub fn append_msg(&mut self, msg: &str) {
        match self {
            ServerError::Application(e) => e.append_msg(msg),
            ServerError::Biz(e) => e.append_msg(msg),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("application exception: {0}")]
    Application(#[from] ApplicationException),
    #[error("transport exception: {0}")]
    Transport(#[from] TransportException),
    #[error("protocol exception: {0}")]
    Protocol(#[from] ProtocolException),
    #[error("biz error: {0}")]
    Biz(#[from] BizError),
}

impl ClientError {
    pub fn append_msg(&mut self, msg: &str) {
        match self {
            ClientError::Application(e) => e.append_msg(msg),
            ClientError::Transport(e) => e.append_msg(msg),
            ClientError::Protocol(e) => e.append_msg(msg),
            ClientError::Biz(e) => e.append_msg(msg),
        }
    }
}

impl Retryable for ClientError {
    fn retryable(&self) -> bool {
        if let Self::Transport(_) = self {
            return true;
        }
        false
    }
}

impl From<LoadBalanceError> for ClientError {
    // TODO: use specified error code
    fn from(err: LoadBalanceError) -> Self {
        ClientError::Application(ApplicationException::new(
            ApplicationExceptionKind::INTERNAL_ERROR,
            err.to_string(),
        ))
    }
}

impl From<ThriftException> for ClientError {
    fn from(e: ThriftException) -> Self {
        match e {
            ThriftException::Application(e) => ClientError::Application(e),
            ThriftException::Transport(e) => ClientError::Transport(e),
            ThriftException::Protocol(e) => ClientError::Protocol(e),
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Transport(TransportException::from(e))
    }
}

#[derive(Debug, thiserror::Error, Clone, Default)]
pub struct BizError {
    pub status_code: i32,
    pub status_message: FastStr,
    pub extra: Option<AHashMap<FastStr, FastStr>>,
}

impl Display for BizError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut extra_str = String::new();
        if let Some(extra) = &self.extra {
            for (k, v) in extra {
                extra_str.push_str(&format!("{}: {},", k, v));
            }
        }
        write!(
            f,
            "status_code: {}, status_message: {}, extra: {}",
            self.status_code, self.status_message, extra_str
        )
    }
}

impl BizError {
    pub fn new(status_code: i32, status_message: FastStr) -> Self {
        Self {
            status_code,
            status_message,
            extra: None,
        }
    }

    pub fn with_extra(
        status_code: i32,
        status_message: FastStr,
        extra: AHashMap<FastStr, FastStr>,
    ) -> Self {
        Self {
            status_code,
            status_message,
            extra: Some(extra),
        }
    }

    pub fn append_msg(&mut self, msg: &str) {
        let mut s = String::with_capacity(self.status_message.len() + msg.len());
        s.push_str(self.status_message.as_str());
        s.push_str(msg);
        self.status_message = s.into();
    }
}

impl From<BizError> for ApplicationException {
    fn from(e: BizError) -> Self {
        ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, e.to_string())
    }
}

pub(crate) fn server_error_to_application_exception(e: ServerError) -> ApplicationException {
    match e {
        ServerError::Application(e) => e,
        ServerError::Biz(e) => e.into(),
    }
}

pub(crate) fn thrift_exception_to_application_exception(
    e: pilota::thrift::ThriftException,
) -> ApplicationException {
    match e {
        pilota::thrift::ThriftException::Application(e) => e,
        pilota::thrift::ThriftException::Transport(e) => {
            ApplicationException::new(ApplicationExceptionKind::INTERNAL_ERROR, e.to_string())
        }
        pilota::thrift::ThriftException::Protocol(e) => {
            ApplicationException::new(ApplicationExceptionKind::PROTOCOL_ERROR, e.to_string())
        }
    }
}
