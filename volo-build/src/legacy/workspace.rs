use pilota_build::{IdlService, Plugin};
use volo::FastStr;

use super::{model, util::get_or_download_idl};

pub struct Builder<MkB, P> {
    pilota_builder: pilota_build::Builder<MkB, P>,
}

impl Builder<crate::thrift_backend::MkThriftBackend, crate::parser::ThriftParser> {
    pub fn thrift() -> Self {
        Self {
            pilota_builder: pilota_build::Builder::thrift()
                .with_backend(crate::thrift_backend::MkThriftBackend),
        }
    }
}

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

impl WorkspaceConfig {
    pub fn update_idls(&mut self) -> anyhow::Result<()> {
        self.services.iter_mut().try_for_each(|s| s.idl.update())
    }
}

impl<MkB, P> Builder<MkB, P>
where
    MkB: pilota_build::MakeBackend + Send,
    MkB::Target: Send,
    P: pilota_build::parser::Parser,
{
    pub fn gen(self) {
        let work_dir = std::env::current_dir().unwrap();
        let config = match std::fs::read(work_dir.join("volo.workspace.yml")) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("failed to read volo.workspace.yml file, err: {}", e);
                std::process::exit(1);
            }
        };
        let config = match serde_yaml::from_slice::<WorkspaceConfig>(&config) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("failed to parse volo.workspace.yml, err: {}", e);
                std::process::exit(1);
            }
        };
        let services = config
            .services
            .into_iter()
            .map(|s| {
                get_or_download_idl(s.idl, work_dir.join("target")).map(|idl| IdlService {
                    path: idl.path,
                    config: s.config,
                })
            })
            .collect::<Result<Vec<_>, _>>();
        match services {
            Ok(services) => {
                self.ignore_unused(!config.touch_all)
                    .dedup(config.dedup_list)
                    .common_crate_name(config.common_crate_name)
                    .pilota_builder
                    .compile_with_config(services, pilota_build::Output::Workspace(work_dir));
            }
            Err(e) => {
                eprintln!("failed to get or download idl, err: {}", e);
                std::process::exit(1);
            }
        }
    }

    pub fn plugin(mut self, plugin: impl Plugin + 'static) -> Self {
        self.pilota_builder = self.pilota_builder.plugin(plugin);
        self
    }

    pub fn ignore_unused(mut self, ignore_unused: bool) -> Self {
        self.pilota_builder = self.pilota_builder.ignore_unused(ignore_unused);
        self
    }

    pub fn dedup(mut self, dedup_list: Vec<FastStr>) -> Self {
        self.pilota_builder = self.pilota_builder.dedup(dedup_list);
        self
    }

    pub fn nonstandard_snake_case(mut self, nonstandard_snake_case: bool) -> Self {
        self.pilota_builder = self
            .pilota_builder
            .nonstandard_snake_case(nonstandard_snake_case);
        self
    }

    pub fn common_crate_name(mut self, name: FastStr) -> Self {
        self.pilota_builder = self.pilota_builder.common_crate_name(name);
        self
    }
}
