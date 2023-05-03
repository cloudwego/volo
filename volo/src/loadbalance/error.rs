use motore::BoxError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LoadBalanceError {
    #[error("load balance retry reaches end")]
    Retry,
    #[error("load balance discovery error: {0:?}")]
    Discover(#[from] BoxError),
    #[error("miss request_key for consistent hash balance")]
    MissRequestKey,
}

pub trait Retryable {
    fn retryable(&self) -> bool {
        false
    }
}
