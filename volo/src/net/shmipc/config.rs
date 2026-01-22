use std::sync::LazyLock;

use arc_swap::ArcSwap;
pub use shmipc::{
    config::{Config, SizePercentPair},
    consts::MemMapType,
    session::SessionManagerConfig,
};

pub static DEFAULT_SESSION_MANAGER_CONFIG: LazyLock<ArcSwap<SessionManagerConfig>> =
    LazyLock::new(Default::default);
pub static DEFAULT_SHMIPC_CONFIG: LazyLock<ArcSwap<Config>> = LazyLock::new(Default::default);

tokio::task_local! {
    pub static SESSION_MANAGER_CONFIG: SessionManagerConfig;
    pub static SHMIPC_CONFIG: Config;
}

pub(crate) fn session_manager_config() -> SessionManagerConfig {
    match SESSION_MANAGER_CONFIG.try_get() {
        Ok(conf) => conf,
        _ => DEFAULT_SESSION_MANAGER_CONFIG.load().as_ref().clone(),
    }
}

pub(crate) fn shmipc_config() -> Config {
    match SHMIPC_CONFIG.try_get() {
        Ok(conf) => conf,
        _ => DEFAULT_SHMIPC_CONFIG.load().as_ref().clone(),
    }
}
