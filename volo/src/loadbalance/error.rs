use motore::BoxError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LoadBalanceError {
    #[error("load balance retry reaches end")]
    Retry,
    #[error("load balance discovery error: {0:?}")]
    Discover(#[from] BoxError),
    #[error("missing 'request_hash' for consistent hash load balancer")]
    MissRequestHash,
    #[error("no available instance for load balance")]
    NoAvailableInstance,
}

pub trait Retryable {
    fn retryable(&self) -> bool {
        false
    }
}
