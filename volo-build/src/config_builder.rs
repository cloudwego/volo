use std::path::{Path, PathBuf};

use anyhow::Ok;
use itertools::Itertools;
use pilota_build::BoxClonePlugin;

use crate::{
    model::{GitSource, Source},
    util::{
        download_files_from_git, get_git_path, open_config_file, read_config_from_file, Task,
        DEFAULT_CONFIG_FILE, DEFAULT_DIR,
    },
};

pub struct ConfigBuilder {
    filename: PathBuf,
    plugins: Vec<BoxClonePlugin>,
}

#[allow(clippy::large_enum_variant)]
enum InnerBuilder {
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

    pub fn touch(self, items: impl IntoIterator<Item = (PathBuf, Vec<impl Into<String>>)>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.touch(items)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.touch(items)),
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
        let f = open_config_file(self.filename)?;
        let config = read_config_from_file(&f)?;

        config.entries.into_iter().try_for_each(|(_key, entry)| {
            let mut builder = match entry.protocol {
                crate::model::IdlProtocol::Thrift => InnerBuilder::thrift(),
                crate::model::IdlProtocol::Protobuf => InnerBuilder::protobuf(),
            }
            .filename(entry.filename);

            for p in self.plugins.iter() {
                builder = builder.plugin(p.clone());
            }

            for idl in entry.idls {
                let (path, includes) = if let Source::Git(GitSource {
                    ref repo, ref lock, ..
                }) = idl.source
                {
                    let lock = lock.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "please exec 'volo idl update' or specify the lock for {}",
                            repo
                        )
                    })?;
                    let dir = DEFAULT_DIR.join(get_git_path(repo.as_str())?).join(lock);
                    let task = Task::new(
                        vec![idl.path.to_string_lossy().to_string()],
                        dir.clone(),
                        repo.clone(),
                        lock.to_string(),
                    );
                    download_files_from_git(task)?;

                    (
                        dir.join(&idl.path),
                        idl.includes
                            .unwrap_or_default()
                            .into_iter()
                            .map(|v| dir.join(v))
                            .collect_vec(),
                    )
                } else {
                    (idl.path.to_path_buf(), idl.includes.unwrap_or_default())
                };

                builder = builder
                    .add_service(path.clone())
                    .includes(includes)
                    .touch([(path, idl.touch)]);
            }

            builder.write()?;

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
