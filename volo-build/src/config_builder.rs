use std::path::{Path, PathBuf};

use anyhow::Ok;
use pilota_build::BoxClonePlugin;
use volo::FastStr;

use crate::{
    model::{self, CodegenOption, Entry, GitSource, Source},
    util::{
        get_or_download_idl, open_config_file, read_config_from_file, LocalIdl,
        DEFAULT_CONFIG_FILE, DEFAULT_DIR,
    },
};

pub struct SingleConfigBuilder {
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

    pub fn nonstandard_snake_case(self, nonstandard_snake_case: bool) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.nonstandard_snake_case(nonstandard_snake_case))
            }
            InnerBuilder::Thrift(inner) => {
                InnerBuilder::Thrift(inner.nonstandard_snake_case(nonstandard_snake_case))
            }
        }
    }

    pub fn common_crate_name(self, name: FastStr) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.common_crate_name(name)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.common_crate_name(name)),
        }
    }
}

impl SingleConfigBuilder {
    pub fn new(filename: PathBuf) -> Self {
        SingleConfigBuilder {
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

                for s in entry.services {
                    let repo = if let Source::Git(GitSource { ref repo_name }) = s.idl.source {
                        Some(
                            entry
                                .repos
                                .get(repo_name)
                                .expect("git source requires the repo info for idl"),
                        )
                    } else {
                        None
                    };

                    let target = PathBuf::from(&*DEFAULT_DIR).join(entry_name.clone());
                    let LocalIdl { path, includes } = get_or_download_idl(s.idl, repo, target)?;
                    let CodegenOption {
                        keep_unknown_fields,
                        touch,
                        ..
                    } = s.codegen_option;

                    println!("keep unknown fields switch is: {}", keep_unknown_fields);

                    builder = builder
                        .add_service(path.clone())
                        .includes(includes)
                        .touch([(path.clone(), touch)]);
                    if keep_unknown_fields {
                        builder = builder.keep_unknown_fields([path])
                    }
                }

                builder
                    .ignore_unused(!entry.common_option.touch_all)
                    .nonstandard_snake_case(entry.common_option.nonstandard_snake_case)
                    .write()?;

                Ok(())
            })?;
        Ok(())
    }
}

impl Default for SingleConfigBuilder {
    fn default() -> Self {
        SingleConfigBuilder::new(PathBuf::from(DEFAULT_CONFIG_FILE))
    }
}

pub struct InitBuilder {
    entry_name: String,
    entry: Entry,
}

impl InitBuilder {
    pub fn new(entry_name: String, entry: Entry) -> Self {
        InitBuilder { entry_name, entry }
    }

    pub fn init(self) -> anyhow::Result<(String, String)> {
        let mut builder = match self.entry.protocol {
            model::IdlProtocol::Thrift => InnerBuilder::thrift(),
            model::IdlProtocol::Protobuf => InnerBuilder::protobuf(),
        }
        .filename(self.entry.filename);

        for s in self.entry.services {
            let repo = if let Source::Git(GitSource { ref repo_name }) = s.idl.source {
                Some(
                    self.entry
                        .repos
                        .get(repo_name)
                        .expect("git source requires the repo info for idl"),
                )
            } else {
                None
            };
            let target = PathBuf::from(&*DEFAULT_DIR).join(self.entry_name.clone());
            let LocalIdl { path, includes } = get_or_download_idl(s.idl, repo, target)?;
            let CodegenOption {
                keep_unknown_fields,
                touch,
                ..
            } = s.codegen_option;

            builder = builder
                .add_service(path.clone())
                .includes(includes)
                .touch([(path.clone(), touch)]);
            if keep_unknown_fields {
                builder = builder.keep_unknown_fields([path])
            }
        }

        builder.init_service()
    }
}
