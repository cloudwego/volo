use volo::FastStr;

use super::model;

#[derive(serde::Deserialize, serde::Serialize)]
pub struct Service {
    pub idl: model::Idl,
    #[serde(default)]
    pub config: serde_yaml::Value,
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub(crate) touch_all: bool,
    #[serde(default)]
    pub(crate) dedup_list: Vec<FastStr>,
    #[serde(default)]
    pub(crate) nonstandard_snake_case: bool,
    #[serde(default = "common_crate_name")]
    pub(crate) common_crate_name: FastStr,
    pub(crate) services: Vec<Service>,
}

fn common_crate_name() -> FastStr {
    "common".into()
}
