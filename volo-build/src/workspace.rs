use std::path::PathBuf;

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
pub struct WorkspaceConfig {
    pub(crate) files: Vec<PathBuf>,
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

        self.pilota_builder
            .compile(&config.files, pilota_build::Output::Workspace(work_dir));
    }
}
