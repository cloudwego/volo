use std::{fmt::Display, io};

pub use pilota::thrift::{
    ApplicationException, ApplicationExceptionKind, ProtocolException, ProtocolExceptionKind,
    ThriftException, TransportException, new_application_exception, new_protocol_exception,
};
use pilota::{AHashMap, FastStr};
use volo::loadbalance::error::{LoadBalanceError, Retryable};

pub type ServerResult<T> = Result<T, ServerError>;
pub type ClientResult<T> = Result<T, ClientError>;

#[derive(Debug, thiserror::Error, Clone)]
pub enum ServerError {
    #[error("application exception: {0}")]
    Application(#[from] ApplicationException),
    #[error("biz error: {0}")]
    Biz(#[from] BizError),
}

impl From<anyhow::Error> for ServerError {
    fn from(e: anyhow::Error) -> Self {
        e.downcast::<ServerError>().unwrap_or_else(|e| {
            e.downcast::<ApplicationException>()
                .map(Into::into)
                .unwrap_or_else(|e| {
                    e.downcast::<BizError>()
                        .map(Into::into)
                        .unwrap_or_else(|e| {
                            ServerError::Application(ApplicationException::new(
                                ApplicationExceptionKind::INTERNAL_ERROR,
                                e.to_string(),
                            ))
                        })
                })
        })
    }
}

impl From<ClientError> for ServerError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Application(e) => ServerError::Application(e),
            ClientError::Transport(e) => ServerError::Application(ApplicationException::new(
                ApplicationExceptionKind::INTERNAL_ERROR,
                e.to_string(),
            )),
            ClientError::Protocol(e) => ServerError::Application(ApplicationException::new(
                ApplicationExceptionKind::PROTOCOL_ERROR,
                e.to_string(),
            )),
            ClientError::Biz(e) => ServerError::Biz(e),
        }
    }
}

impl From<ThriftException> for ServerError {
    fn from(e: ThriftException) -> Self {
        thrift_exception_to_application_exception(e).into()
    }
}

impl From<ProtocolException> for ServerError {
    fn from(e: ProtocolException) -> Self {
        ServerError::Application(ApplicationException::new(
            ApplicationExceptionKind::PROTOCOL_ERROR,
            e.to_string(),
        ))
    }
}

impl From<TransportException> for ServerError {
    fn from(e: TransportException) -> Self {
        ServerError::Application(ApplicationException::new(
            ApplicationExceptionKind::INTERNAL_ERROR,
            e.to_string(),
        ))
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

#[derive(Debug, thiserror::Error, Clone)]
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
                extra_str.push_str(&format!("{k}: {v},"));
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

pub enum MaybeException<T, E> {
    Ok(T),
    Exception(E),
}

impl<T: Default, E> Default for MaybeException<T, E> {
    fn default() -> Self {
        MaybeException::Ok(Default::default())
    }
}
