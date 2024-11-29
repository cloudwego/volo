use std::path::PathBuf;

use pilota_build::{IdlService, Plugin};
use volo::FastStr;

use crate::{
    model::WorkspaceConfig,
    util::{download_repos_to_target, get_idl_build_path_and_includes, ServiceBuilder},
};

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

impl Builder<crate::grpc_backend::MkGrpcBackend, crate::parser::ProtobufParser> {
    pub fn protobuf() -> Self {
        Self {
            pilota_builder: pilota_build::Builder::protobuf()
                .with_backend(crate::grpc_backend::MkGrpcBackend),
        }
    }
}

impl WorkspaceConfig {
    pub fn update_repos(&mut self) -> anyhow::Result<()> {
        self.repos
            .iter_mut()
            .try_for_each(|(_, repo)| repo.update())
    }
}

impl<MkB, P> Builder<MkB, P>
where
    MkB: pilota_build::MakeBackend + Send,
    MkB::Target: Send,
    P: pilota_build::parser::Parser,
{
    pub fn gen(mut self) {
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

        let target_dir = work_dir.join("target");
        let repo_dir_map = if let Ok(repo_dir_map) =
            download_repos_to_target(&config.repos, target_dir.as_path())
        {
            repo_dir_map
        } else {
            eprintln!("failed to download repos");
            std::process::exit(1);
        };

        let (idl_services, service_builders): (Vec<_>, Vec<_>) = config
            .services
            .into_iter()
            .map(|s| {
                let (path, includes) = get_idl_build_path_and_includes(&s.idl, &repo_dir_map);
                (
                    IdlService {
                        path: path.clone(),
                        config: s.codegen_option.config,
                    },
                    ServiceBuilder {
                        path,
                        includes,
                        touch: s.codegen_option.touch,
                        keep_unknown_fields: s.codegen_option.keep_unknown_fields,
                    },
                )
            })
            .unzip();
        for ServiceBuilder {
            path,
            includes,
            touch,
            keep_unknown_fields,
        } in service_builders
        {
            self = self.include_dirs(includes).touch([(path.clone(), touch)]);
            if keep_unknown_fields {
                self = self.keep_unknown_fields([path]);
            }
        }
        self.ignore_unused(!config.common_option.touch_all)
            .dedup(config.common_option.dedups)
            .special_namings(config.common_option.special_namings)
            .common_crate_name(config.common_crate_name)
            .split_generated_files(config.common_option.split_generated_files)
            .pilota_builder
            .compile_with_config(idl_services, pilota_build::Output::Workspace(work_dir));
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

    pub fn common_crate_name(mut self, name: FastStr) -> Self {
        self.pilota_builder = self.pilota_builder.common_crate_name(name);
        self
    }

    pub fn special_namings(mut self, namings: impl IntoIterator<Item = FastStr>) -> Self {
        self.pilota_builder = self.pilota_builder.special_namings(namings);
        self
    }

    pub fn include_dirs(mut self, include_dirs: Vec<PathBuf>) -> Self {
        self.pilota_builder = self.pilota_builder.include_dirs(include_dirs);
        self
    }

    pub fn keep_unknown_fields(
        mut self,
        keep_unknown_fields: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        self.pilota_builder = self.pilota_builder.keep_unknown_fields(keep_unknown_fields);
        self
    }

    pub fn split_generated_files(mut self, split_generated_files: bool) -> Self {
        self.pilota_builder = self
            .pilota_builder
            .split_generated_files(split_generated_files);
        self
    }

    pub fn touch(
        mut self,
        items: impl IntoIterator<Item = (PathBuf, Vec<impl Into<String>>)>,
    ) -> Self {
        self.pilota_builder = self.pilota_builder.touch(items);
        self
    }
}
