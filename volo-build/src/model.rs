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

/// Resolve one transport and shared-data policy for an entire Thrift entry.
/// An omitted setting inherits the entry's explicit setting; two different
/// explicit settings are rejected, including those on data-only includes.
pub(crate) fn thrift_grpc_options(
    services: &[Service],
) -> anyhow::Result<Option<crate::thrift_grpc_backend::Options>> {
    resolve_thrift_grpc_configs(
        services
            .iter()
            .map(|service| &service.codegen_option.config),
        false,
    )
}

pub(crate) fn resolve_thrift_grpc_configs<'a>(
    configs: impl IntoIterator<Item = &'a serde_yaml::Value>,
    explicit_grpc_builder: bool,
) -> anyhow::Result<Option<crate::thrift_grpc_backend::Options>> {
    let configs: Vec<_> = configs.into_iter().collect();
    let mut transport: Option<String> = None;
    for config in &configs {
        let Some(value) = config.get("rpc_transport") else {
            continue;
        };
        let value = value
            .as_str()
            .ok_or_else(|| anyhow!("rpc_transport must be a string"))?;
        if !matches!(value, "grpc" | "ttheader") {
            anyhow::bail!("unsupported Thrift rpc_transport: {value}; expected grpc or ttheader");
        }
        if transport.as_deref().is_some_and(|old| old != value)
            || (explicit_grpc_builder && value != "grpc")
        {
            anyhow::bail!("conflicting rpc_transport settings in the same Thrift generation entry");
        }
        transport = Some(value.to_owned());
    }
    // Existing plugins own arbitrary YAML config values, including scalars.
    // Do not interpret new option names unless this entry explicitly uses gRPC.
    if !explicit_grpc_builder && transport.as_deref() != Some("grpc") {
        return Ok(None);
    }
    #[derive(Deserialize, Default)]
    struct Config {
        shared_types_module: Option<String>,
        wire_package: Option<String>,
        runtime_crate: Option<String>,
        #[serde(default)]
        wire_services: std::collections::BTreeMap<String, String>,
    }
    let mut options = crate::thrift_grpc_backend::Options::default();
    for value in configs {
        let config: Config = if value.is_mapping() {
            serde_yaml::from_value(value.clone())?
        } else {
            Config::default()
        };
        for (target, source, name) in [
            (
                &mut options.shared_types_module,
                config.shared_types_module,
                "shared_types_module",
            ),
            (
                &mut options.wire_package,
                config.wire_package,
                "wire_package",
            ),
            (
                &mut options.runtime_crate,
                config.runtime_crate,
                "runtime_crate",
            ),
        ] {
            if let Some(source) = source {
                if target.as_ref().is_some_and(|old| old != &source) {
                    anyhow::bail!(
                        "conflicting {name} settings in the same Thrift generation entry"
                    );
                }
                *target = Some(source);
            }
        }
        for (key, value) in config.wire_services {
            if options
                .wire_services
                .insert(key.clone(), value.clone())
                .is_some_and(|old| old != value)
            {
                anyhow::bail!("conflicting wire service override for {key}");
            }
        }
    }
    Ok(Some(options))
}

impl CodegenOption {
    fn is_empty(&self) -> bool {
        self.crate_name.is_none()
            && self.touch.is_empty()
            && !self.keep_unknown_fields
            && self.config.is_null()
    }

    pub(crate) fn no_service(&self) -> bool {
        self.config
            .get("no_service")
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false)
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
