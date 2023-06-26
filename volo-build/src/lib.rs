#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]

use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::anyhow;
use itertools::Itertools;
use pilota_build::{parser::Parser, IdlService};

pub mod config_builder;
pub mod grpc_backend;
pub mod model;
pub mod thrift_backend;
pub mod util;
pub mod workspace;

pub use config_builder::ConfigBuilder;
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

impl Builder<thrift_backend::MkThriftBackend, parser::ThriftParser> {
    pub fn thrift() -> Self {
        Builder {
            pilota_builder: pilota_build::Builder::thrift()
                .with_backend(thrift_backend::MkThriftBackend),
            out_dir: Default::default(),
            filename: "volo_gen".into(),
            idls: Default::default(),
            config_file_path: "volo.yml".into(),
        }
    }
}

impl Builder<grpc_backend::MkGrpcBackend, parser::ProtobufParser> {
    pub fn protobuf() -> Self {
        Builder {
            pilota_builder: pilota_build::Builder::protobuf()
                .with_backend(grpc_backend::MkGrpcBackend),
            out_dir: Default::default(),
            filename: "volo_gen".into(),
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
    P: Parser + Default,
{
    pub fn include_dirs(mut self, include_dirs: Vec<PathBuf>) -> Self {
        self.pilota_builder = self.pilota_builder.include_dirs(include_dirs);
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

    pub fn init_service(self) -> Option<(String, String)> {
        assert_eq!(self.idls.len(), 1);
        self.pilota_builder.init_service(
            self.idls
                .into_iter()
                .map(IdlService::from_path)
                .nth(0)
                .unwrap(),
        )
    }
}

macro_rules! join_multi_strs {
    ($sep: tt, |$($s: tt),*| ->  $f: tt) => {
        {
            #[allow(unused_parens)]
            itertools::izip!($(&$s),*).map(|($($s),*)| format!($f)).join($sep)
        }
    };
}

pub(crate) use join_multi_strs;
