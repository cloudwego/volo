use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::anyhow;
use itertools::Itertools;
use pilota_build::{parser::Parser, IdlService};

pub mod model;
pub mod util;
pub mod workspace;

pub use pilota_build::{
    parser, plugin, rir, BoxClonePlugin, ClonePlugin, Context, DefId, MakeBackend, Plugin,
};

pub struct Builder<MkB, P> {
    pilota_builder: pilota_build::Builder<MkB, P>,
    idls: Vec<PathBuf>,
    out_dir: Option<PathBuf>,
    filename: PathBuf,
    config_file_path: PathBuf,
}

impl Builder<crate::thrift_backend::MkThriftBackend, parser::ThriftParser> {
    pub fn thrift() -> Self {
        Builder {
            pilota_builder: pilota_build::Builder::thrift()
                .with_backend(crate::thrift_backend::MkThriftBackend),
            out_dir: Default::default(),
            filename: "volo_gen.rs".into(),
            idls: Default::default(),
            config_file_path: "volo.yml".into(),
        }
    }
}

impl Builder<crate::grpc_backend::MkGrpcBackend, parser::ProtobufParser> {
    pub fn protobuf() -> Self {
        Builder {
            pilota_builder: pilota_build::Builder::protobuf()
                .with_backend(crate::grpc_backend::MkGrpcBackend),
            out_dir: Default::default(),
            filename: "volo_gen.rs".into(),
            idls: Default::default(),
            config_file_path: "volo.yml".into(),
        }
    }
}

impl<MkB, Parser> Builder<MkB, Parser> {
    pub fn add_service<P>(mut self, path: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.idls.push(path.as_ref().into());

        self
    }

    pub fn plugin<P: Plugin + 'static>(mut self, p: P) -> Self {
        self.pilota_builder = self.pilota_builder.plugin(p);

        self
    }

    /// the generated filename
    pub fn filename(mut self, filename: PathBuf) -> Self {
        self.filename = filename;
        self
    }

    pub fn out_dir<P: AsRef<Path>>(mut self, out_dir: P) -> Self {
        self.out_dir = Some(out_dir.as_ref().to_path_buf());
        self
    }

    pub fn config_file_path(mut self, path: PathBuf) -> Self {
        self.config_file_path = path;
        self
    }

    pub fn ignore_unused(mut self, ignore_unused: bool) -> Self {
        self.pilota_builder = self.pilota_builder.ignore_unused(ignore_unused);
        self
    }

    pub fn touch(
        mut self,
        items: impl IntoIterator<Item = (PathBuf, Vec<impl Into<String>>)>,
    ) -> Self {
        self.pilota_builder = self.pilota_builder.touch(items);
        self
    }

    pub fn keep_unknown_fields(
        mut self,
        keep_unknown_fields: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        self.pilota_builder = self.pilota_builder.keep_unknown_fields(keep_unknown_fields);
        self
    }

    fn get_out_dir(&self) -> anyhow::Result<PathBuf> {
        self.out_dir
            .clone()
            .or_else(|| {
                std::env::var("OUT_DIR")
                    .ok()
                    .and_then(|dir| PathBuf::from_str(&dir).ok())
            })
            .ok_or_else(|| anyhow!("please specify out_dir"))
    }
}

impl<MkB, P> Builder<MkB, P>
where
    MkB: MakeBackend + Send,
    MkB::Target: Send,
    P: Parser,
{
    pub fn include_dirs(mut self, include_dirs: Vec<PathBuf>) -> Self {
        self.pilota_builder = self.pilota_builder.include_dirs(include_dirs);
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

    pub fn write(self) -> anyhow::Result<()> {
        let out_dir = self.get_out_dir()?;

        if !out_dir.exists() {
            std::fs::create_dir_all(&out_dir)?;
        }

        if self.idls.is_empty() {
            return Ok(());
        }

        self.pilota_builder.compile_with_config(
            self.idls
                .into_iter()
                .map(IdlService::from_path)
                .collect_vec(),
            pilota_build::Output::File(out_dir.join(self.filename)),
        );
        Ok(())
    }

    pub fn init_service(self) -> anyhow::Result<(String, String)> {
        assert_eq!(self.idls.len(), 1);
        self.pilota_builder.init_service(
            self.idls
                .into_iter()
                .map(IdlService::from_path)
                .next()
                .unwrap(),
        )
    }
}
use volo::FastStr;
