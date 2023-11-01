use std::{collections::HashMap, path::PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use crate::util::get_repo_latest_commit_id;

pub const DEFAULT_ENTRY_NAME: &str = "default";
pub const DEFAULT_FILENAME: &str = "volo_gen.rs";

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub entries: HashMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Entry {
    pub protocol: IdlProtocol,
    pub filename: PathBuf,

    pub idls: Vec<Idl>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdlProtocol {
    #[serde(rename = "thrift")]
    Thrift,
    #[serde(rename = "protobuf")]
    Protobuf,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Idl {
    #[serde(flatten)]
    pub source: Source,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub includes: Option<Vec<PathBuf>>,
    #[serde(skip_serializing_if = "Vec::is_empty", default = "Vec::new")]
    pub touch: Vec<String>,
    #[serde(default = "default_keep_unknown_fields")]
    pub keep_unknown_fields: bool,
}

impl Idl {
    pub fn update(&mut self) -> anyhow::Result<()> {
        match &mut self.source {
            Source::Git(git_source) => git_source.update(),
            Source::Local => Ok(()),
        }
    }

    pub fn ensure_readable(&self) -> anyhow::Result<()> {
        // We should ensure that:
        //   1. All the files exist (`ENOENT` may occur)
        //   2. All the files can be accessed by the current user (`EPERM` may occur)
        //   3. All the files can be read by the current user (`EPERM` may occur)
        // The simplest method is opening it with read perm (`O_RDONLY`)

        try_open_readonly(&self.path)
            .map_err(|e| anyhow!("{}: {}", self.path.to_str().unwrap(), e))?;

        if let Some(includes) = &self.includes {
            for inc in includes.iter() {
                try_open_readonly(inc).map_err(|e| anyhow!("{}: {}", inc.to_str().unwrap(), e))?;
            }
        }

        Ok(())
    }
}

fn default_keep_unknown_fields() -> bool {
    false
}

fn try_open_readonly<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<()> {
    let _ = std::fs::OpenOptions::new().read(true).open(path)?;
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
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
}

impl GitSource {
    pub fn update(&mut self) -> anyhow::Result<()> {
        let commit_id =
            get_repo_latest_commit_id(&self.repo, self.r#ref.as_deref().unwrap_or("HEAD"))?;

        let _ = self.lock.insert(commit_id);

        Ok::<(), anyhow::Error>(())
    }
}

impl Config {
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
            includes: None,
            touch: Vec::default(),
            keep_unknown_fields: false,
        }
    }

    pub fn protocol(&self) -> IdlProtocol {
        match self.path.extension().and_then(|v| v.to_str()) {
            Some("thrift") => IdlProtocol::Thrift,
            Some("proto") => IdlProtocol::Protobuf,
            _ => {
                eprintln!("invalid file ext {:?}", self.path);
                std::process::exit(1);
            }
        }
    }
}
