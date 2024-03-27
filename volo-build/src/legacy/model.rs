use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

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
    #[serde(default)]
    pub touch_all: bool,
    #[serde(default)]
    pub nonstandard_snake_case: bool,
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
    #[serde(skip_serializing_if = "Vec::is_empty", default = "Vec::new")]
    pub includes: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty", default = "Vec::new")]
    pub touch: Vec<String>,
    #[serde(skip_serializing_if = "is_false", default)]
    pub keep_unknown_fields: bool,
}

fn is_false(b: &bool) -> bool {
    !b
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

impl Config {
    pub fn new() -> Self {
        Default::default()
    }
}
