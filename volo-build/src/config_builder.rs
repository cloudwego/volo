use std::path::{Path, PathBuf};

use anyhow::Ok;
use pilota_build::BoxClonePlugin;
use volo::FastStr;

use crate::{
    model::{self, Entry},
    util::{
        download_repos_to_target, get_service_builders_from_services, open_config_file,
        read_config_from_file, ServiceBuilder, DEFAULT_CONFIG_FILE, DEFAULT_DIR,
    },
};

pub struct ConfigBuilder {
    filename: PathBuf,
    plugins: Vec<BoxClonePlugin>,
}

#[allow(clippy::large_enum_variant)]
pub enum InnerBuilder {
    Protobuf(
        crate::Builder<crate::grpc_backend::MkGrpcBackend, pilota_build::parser::ProtobufParser>,
    ),
    Thrift(
        crate::Builder<crate::thrift_backend::MkThriftBackend, pilota_build::parser::ThriftParser>,
    ),
}

impl InnerBuilder {
    fn thrift() -> Self {
        InnerBuilder::Thrift(crate::Builder::thrift())
    }

    fn protobuf() -> Self {
        InnerBuilder::Protobuf(crate::Builder::protobuf())
    }

    fn plugin<P: pilota_build::Plugin + 'static>(self, p: P) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.plugin(p)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.plugin(p)),
        }
    }

    fn write(self) -> anyhow::Result<()> {
        match self {
            InnerBuilder::Protobuf(inner) => inner.write(),
            InnerBuilder::Thrift(inner) => inner.write(),
        }
    }

    fn init_service(self) -> anyhow::Result<(String, String)> {
        match self {
            InnerBuilder::Protobuf(inner) => inner.init_service(),
            InnerBuilder::Thrift(inner) => inner.init_service(),
        }
    }

    fn filename(self, filename: PathBuf) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.filename(filename)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.filename(filename)),
        }
    }

    fn includes(self, includes: Vec<PathBuf>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.include_dirs(includes)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.include_dirs(includes)),
        }
    }

    pub fn add_service<P>(self, path: P) -> Self
    where
        P: AsRef<Path>,
    {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.add_service(path)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.add_service(path)),
        }
    }

    pub fn add_services(mut self, service_builders: Vec<ServiceBuilder>) -> Self {
        for ServiceBuilder {
            path,
            includes,
            touch,
            keep_unknown_fields,
        } in service_builders
        {
            self = self
                .add_service(path.clone())
                .includes(includes)
                .touch([(path.clone(), touch)]);
            if keep_unknown_fields {
                self = self.keep_unknown_fields([path])
            }
        }
        self
    }

    pub fn touch(self, items: impl IntoIterator<Item = (PathBuf, Vec<impl Into<String>>)>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.touch(items)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.touch(items)),
        }
    }

    pub fn ignore_unused(self, ignore_unused: bool) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.ignore_unused(ignore_unused))
            }
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.ignore_unused(ignore_unused)),
        }
    }

    pub fn keep_unknown_fields(self, keep: impl IntoIterator<Item = PathBuf>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.keep_unknown_fields(keep))
            }
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.keep_unknown_fields(keep)),
        }
    }

    pub fn common_crate_name(self, name: FastStr) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.common_crate_name(name)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.common_crate_name(name)),
        }
    }

    pub fn special_namings(self, namings: impl IntoIterator<Item = FastStr>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.special_namings(namings)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.special_namings(namings)),
        }
    }
}

impl ConfigBuilder {
    pub fn new(filename: PathBuf) -> Self {
        ConfigBuilder {
            filename,
            plugins: Vec::new(),
        }
    }

    pub fn plugin<P: pilota_build::ClonePlugin + 'static>(mut self, p: P) -> Self {
        self.plugins.push(BoxClonePlugin::new(p));

        self
    }

    pub fn write(self) -> anyhow::Result<()> {
        println!("cargo:rerun-if-changed={}", self.filename.display());
        let f = open_config_file(self.filename.clone())?;
        let config = read_config_from_file(&f)?;
        config
            .entries
            .into_iter()
            .try_for_each(|(entry_name, entry)| {
                let mut builder = match entry.protocol {
                    model::IdlProtocol::Thrift => InnerBuilder::thrift(),
                    model::IdlProtocol::Protobuf => InnerBuilder::protobuf(),
                }
                .filename(entry.filename.clone());

                for p in self.plugins.iter() {
                    builder = builder.plugin(p.clone());
                }

                // download repos and get the repo paths
                let target_dir = PathBuf::from(&*DEFAULT_DIR).join(entry_name);
                let repo_dir_map = download_repos_to_target(&entry.repos, target_dir)?;

                // get idl builders from services
                let service_builders =
                    get_service_builders_from_services(&entry.services, &repo_dir_map);

                // add build options to the builder and build
                builder
                    .add_services(service_builders)
                    .ignore_unused(!entry.common_option.touch_all)
                    .special_namings(entry.common_option.special_namings)
                    .write()?;

                Ok(())
            })?;
        Ok(())
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        ConfigBuilder::new(PathBuf::from(DEFAULT_CONFIG_FILE))
    }
}

pub struct InitBuilder {
    entry: Entry,
}

impl InitBuilder {
    pub fn new(entry: Entry) -> Self {
        InitBuilder { entry }
    }

    pub fn init(self) -> anyhow::Result<(String, String)> {
        let mut builder = match self.entry.protocol {
            model::IdlProtocol::Thrift => InnerBuilder::thrift(),
            model::IdlProtocol::Protobuf => InnerBuilder::protobuf(),
        }
        .filename(self.entry.filename);

        // download repos and get the repo paths
        let temp_target_dir = tempfile::TempDir::new()?;
        let repo_dir_map = download_repos_to_target(&self.entry.repos, temp_target_dir.as_ref())?;

        // get idl builders from services
        let idl_builders = get_service_builders_from_services(&self.entry.services, &repo_dir_map);

        // add services to the builder
        builder = builder.add_services(idl_builders);

        builder.init_service()
    }
}
