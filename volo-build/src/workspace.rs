use pilota_build::{IdlService, Plugin};

use crate::{model, util::get_or_download_idl};

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
    pub(crate) services: Vec<Service>,
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
        let config = std::fs::read(work_dir.join("volo.workspace.yml")).unwrap();
        let config = serde_yaml::from_slice::<WorkspaceConfig>(&config).unwrap();

        let services = config
            .services
            .into_iter()
            .map(|s| {
                get_or_download_idl(s.idl, work_dir.join("target")).map(|idl| IdlService {
                    path: idl.path,
                    config: s.config,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        self.ignore_unused(!config.touch_all)
            .pilota_builder
            .compile_with_config(services, pilota_build::Output::Workspace(work_dir));
    }

    pub fn plugin(mut self, plugin: impl Plugin + 'static) -> Self {
        self.pilota_builder = self.pilota_builder.plugin(plugin);
        self
    }

    pub fn ignore_unused(mut self, ignore_unused: bool) -> Self {
        self.pilota_builder = self.pilota_builder.ignore_unused(ignore_unused);
        self
    }
}
