use std::{collections::HashMap, path::PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use volo::FastStr;

use crate::util::git::get_repo_latest_commit_id;

pub const DEFAULT_ENTRY_NAME: &str = "default";
pub const DEFAULT_FILENAME: &str = "volo_gen.rs";

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct SingleConfig {
    pub entries: HashMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CommonOption {
    #[serde(default, skip_serializing_if = "is_false")]
    pub touch_all: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dedups: Vec<FastStr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub special_namings: Vec<FastStr>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub split_generated_files: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub with_descriptor: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub with_field_mask: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub with_comments: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Entry {
    pub filename: PathBuf,
    pub protocol: IdlProtocol,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub repos: HashMap<FastStr, Repo>,
    pub services: Vec<Service>,
    #[serde(flatten)]
    pub common_option: CommonOption,
}

fn common_crate_name() -> FastStr {
    "common".into()
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkspaceConfig {
    #[serde(default = "common_crate_name")]
    pub common_crate_name: FastStr,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub repos: HashMap<FastStr, Repo>,
    pub services: Vec<Service>,
    #[serde(flatten)]
    pub common_option: CommonOption,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Repo {
    pub url: FastStr,
    pub r#ref: FastStr,
    pub lock: FastStr,
}

impl Repo {
    pub fn update(&mut self) -> anyhow::Result<()> {
        let commit_id = get_repo_latest_commit_id(&self.url, &self.r#ref)?;
        self.lock = commit_id.into();
        Ok::<(), anyhow::Error>(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Service {
    pub idl: Idl,
    #[serde(default, skip_serializing_if = "CodegenOption::is_empty")]
    pub codegen_option: CodegenOption,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdlProtocol {
    #[serde(rename = "thrift")]
    Thrift,
    #[serde(rename = "protobuf")]
    Protobuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CodegenOption {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crate_name: Option<FastStr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touch: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_unknown_fields: bool,
    #[serde(default, skip_serializing_if = "serde_yaml::Value::is_null")]
    pub config: serde_yaml::Value,
}

impl CodegenOption {
    fn is_empty(&self) -> bool {
        self.crate_name.is_none()
            && self.touch.is_empty()
            && !self.keep_unknown_fields
            && self.config.is_null()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Idl {
    #[serde(flatten)]
    pub source: Source,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Vec::is_empty", default = "Vec::new")]
    pub includes: Vec<PathBuf>,
}

impl Idl {
    pub fn ensure_readable(&self) -> anyhow::Result<()> {
        // We should ensure that:
        //   1. All the files exist (`ENOENT` may occur)
        //   2. All the files can be accessed by the current user (`EPERM` may occur)
        //   3. All the files can be read by the current user (`EPERM` may occur)
        // The simplest method is opening it with read perm (`O_RDONLY`)

        try_open_readonly(&self.path)
            .map_err(|e| anyhow!("{}: {}", self.path.to_str().unwrap(), e))?;

        for inc in self.includes.iter() {
            try_open_readonly(inc).map_err(|e| anyhow!("{}: {}", inc.to_str().unwrap(), e))?;
        }

        Ok(())
    }
}

fn is_false(b: &bool) -> bool {
    !b
}

fn try_open_readonly<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<()> {
    let md = std::fs::metadata(&path)?;
    if md.is_dir() {
        std::fs::read_dir(path)?;
    } else {
        std::fs::OpenOptions::new().read(true).open(path)?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "source")]
pub enum Source {
    #[serde(rename = "git")]
    Git(GitSource),
    #[serde(rename = "local")]
    Local,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GitSource {
    pub repo: FastStr,
}

impl SingleConfig {
    pub fn new() -> Self {
        Default::default()
    }
}

impl Default for Idl {
    fn default() -> Self {
        Self::new()
    }
}

impl Idl {
    pub fn new() -> Self {
        Self {
            source: Source::Local,
            path: PathBuf::from(""),
            includes: Vec::new(),
        }
    }
}
