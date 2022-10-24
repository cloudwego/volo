use std::{fs::create_dir_all, path::PathBuf};

use anyhow::Context;
use clap::{value_parser, Parser};
use heck::ToUpperCamelCase;
use itertools::Itertools;
use lazy_static::lazy_static;
use pilota_thrift_parser::parser::Parser as _;
use regex::Regex;
use volo_build::{
    model::{Entry, GitSource, Idl, Source, DEFAULT_FILENAME},
    util::{get_repo_latest_commit_id, git_archive, DEFAULT_CONFIG_FILE},
};

use crate::command::CliCommand;

#[derive(Parser, Debug)]
#[command(about = "init your project")]
pub struct Init {
    pub name: String,
    #[arg(
        short = 'g',
        long = "git",
        help = "Specify the git repo for idl.\nShould be in the format of \
                \"git@domain:path/repo.git\".\nExample: git@github.com:cloudwego/volo.git"
    )]
    pub git: Option<String>,
    #[arg(
        short = 'r',
        long = "ref",
        requires = "git",
        help = "Specify the git repo ref(commit, branch) for idl.\nExample: main / $TAG / \
                $COMMIT_HASH"
    )]
    pub r#ref: Option<String>,

    #[arg(
        short = 'i',
        long = "includes",
        help = "Specify the include dirs for idl.\nIf -g or --git is specified, then this should \
                be the path in the specified git repo."
    )]
    pub includes: Option<Vec<PathBuf>>,
    #[arg(
        value_parser = value_parser!(PathBuf),
        help = "Specify the path for idl.\nIf -g or --git is specified, then this should be the \
                path in the specified git repo.\nExample: \t-g not \
                specified:\t./idl/server.thrift\n\t\t-g specified:\t\t/path/to/idl/server.thrift"
    )]
    pub idl: PathBuf,
}

impl Init {
    pub fn is_grpc_project(&self) -> bool {
        if let Some(ext) = self.idl.extension() {
            ext == "proto"
        } else {
            false
        }
    }

    fn parse_grpc(&self, contents: String) -> anyhow::Result<(String, String)> {
        lazy_static! {
            static ref PACKAGE_NAME: Regex = Regex::new(r"package\s+([^;]+);").unwrap();
            static ref SERVICE_NAME: Regex = Regex::new(r"service\s+(\w+)\s+\{").unwrap();
        }
        let package_name = PACKAGE_NAME
            .captures(&contents)
            .and_then(|cap| cap.get(1))
            .ok_or_else(|| anyhow::anyhow!("package name not found"))?;
        let namespace = package_name
            .as_str()
            .split('.')
            .collect::<Vec<_>>()
            .join("::");
        let service_name = SERVICE_NAME
            .captures_iter(&contents)
            .last()
            .and_then(|cap| cap.get(1))
            .ok_or_else(|| anyhow::anyhow!("service name not found"))?;
        Ok((service_name.as_str().into(), namespace))
    }

    fn parse_thrift(&self, contents: String) -> anyhow::Result<(String, Option<String>)> {
        let (_remain, res) =
            pilota_thrift_parser::File::parse(contents.as_str()).expect("parse thrift idl failed");
        let service = res
            .items
            .iter()
            .filter_map(|item| match item {
                pilota_thrift_parser::Item::Service(s) => Some(s),
                _ => None,
            })
            .last();

        if service.is_none() {
            return Err(anyhow::anyhow!("no service found!"));
        }

        // try to get and parse idl file, to get the last service's name
        let service_name = service.unwrap().name.to_upper_camel_case();

        let namespace = res
            .package
            .map(|p| p.segments.iter().map(|s| &**s).join("::"))
            .or_else(|| {
                Some(
                    res.path
                        .file_name()?
                        .to_string_lossy()
                        .trim_end_matches(".rs")
                        .to_string(),
                )
            });
        Ok((service_name, namespace))
    }

    fn copy_grpc_template(&self, contents: String) -> anyhow::Result<()> {
        let (service_name, namespace) = self.parse_grpc(contents)?;

        let name = self.name.replace('.', "_").replace('-', "_");
        let cwd = std::env::current_dir()?;
        let folder = cwd.as_path();

        // root dirs
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/rust-toolchain_toml",
            "rust-toolchain.toml"
        );
        crate::templates_to_target_file!(folder, "templates/grpc/gitignore", ".gitignore");
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/cargo_toml",
            "Cargo.toml",
            name = &name
        );

        // src dirs
        std::fs::create_dir_all(folder.join("src/bin"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/src/bin/server_rs",
            "src/bin/server.rs",
            name = &name,
            service_name = &service_name,
            namespace = &namespace,
        );
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/src/lib_rs",
            "src/lib.rs",
            service_name = &service_name,
            namespace = &namespace,
        );

        // volo-gen dirs
        std::fs::create_dir_all(folder.join("volo-gen/src"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/volo-gen/build_rs",
            "volo-gen/build.rs"
        );
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/volo-gen/cargo_toml",
            "volo-gen/Cargo.toml",
        );
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/volo-gen/src/lib_rs",
            "volo-gen/src/lib.rs",
        );

        Ok(())
    }

    fn copy_thrift_template(&self, contents: String, filename: &str) -> anyhow::Result<()> {
        let (service_name, mut namespace) = self.parse_thrift(contents)?;
        let name = self.name.replace('.', "_").replace('-', "_");
        let cwd = std::env::current_dir()?;
        let folder = cwd.as_path();
        if namespace.is_none() {
            namespace = Some(filename.to_string());
        }
        let namespace = unsafe { namespace.unwrap_unchecked() };

        // root dirs
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/rust-toolchain_toml",
            "rust-toolchain.toml"
        );
        crate::templates_to_target_file!(folder, "templates/thrift/gitignore", ".gitignore");
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/cargo_toml",
            "Cargo.toml",
            name = &name
        );

        // src dirs
        std::fs::create_dir_all(folder.join("src/bin"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/src/bin/server_rs",
            "src/bin/server.rs",
            name = &name,
            service_name = &service_name,
            namespace = &namespace,
        );
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/src/lib_rs",
            "src/lib.rs",
            service_name = &service_name,
            namespace = &namespace,
        );

        // volo-gen dirs
        std::fs::create_dir_all(folder.join("volo-gen/src"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/volo-gen/build_rs",
            "volo-gen/build.rs"
        );
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/volo-gen/cargo_toml",
            "volo-gen/Cargo.toml",
        );
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/volo-gen/src/lib_rs",
            "volo-gen/src/lib.rs",
        );

        Ok(())
    }
}

impl CliCommand for Init {
    fn run(&self, cx: crate::context::Context) -> anyhow::Result<()> {
        volo_build::util::with_config(|config| {
            let mut lock = None;

            let contents = if self.git.is_some() {
                let cwd = std::env::current_dir()?.join("target");
                create_dir_all(&cwd).context("create target dir")?;
                let r#ref = self.r#ref.as_deref().unwrap_or("HEAD");
                let _ = lock.insert(get_repo_latest_commit_id(
                    self.git.as_ref().unwrap(),
                    r#ref,
                )?);

                git_archive(
                    self.git.as_ref().unwrap().as_str(),
                    lock.as_ref().unwrap(),
                    cwd.as_path(),
                )
                .context("git archive")?;
                std::fs::read_to_string(cwd.join(&self.idl)).context("read idl")?
            } else {
                std::fs::read_to_string(&self.idl).context("read idl")?
            };

            if self.is_grpc_project() {
                self.copy_grpc_template(contents)?;
            } else {
                self.copy_thrift_template(
                    contents,
                    self.idl.file_name().unwrap().to_str().unwrap(),
                )?;
            }

            let mut idl = Idl::new();
            idl.includes = self.includes.clone();
            if let Some(git) = self.git.as_ref() {
                idl.source = Source::Git(GitSource {
                    repo: git.clone(),
                    r#ref: None,
                    lock,
                });
                idl.path = self.idl.clone();
            } else {
                // we will move volo.yml to volo-gen, so we need to add .. to includes and idl path
                if let Some(includes) = &mut idl.includes {
                    for i in includes {
                        if i.is_absolute() {
                            continue;
                        }
                        *i = PathBuf::new().join("../").join(i.clone());
                    }
                }
                if self.idl.is_absolute() {
                    idl.path = self.idl.clone();
                } else {
                    idl.path = PathBuf::new().join("../").join(self.idl.clone());
                }
            }

            let entry = config.entries.entry(cx.entry_name);
            match entry {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    // find the specified idl and update it.
                    let mut found = false;
                    for idl in e.get_mut().idls.iter_mut() {
                        if self.idl != idl.path {
                            continue;
                        }
                        match idl.source {
                            Source::Git(GitSource {
                                ref mut repo,
                                ref mut r#ref,
                                ..
                            }) if self.git.is_some() => {
                                // found the desired idl, update it
                                found = true;
                                if self.git.is_some() {
                                    *repo = self.git.as_ref().unwrap().clone();
                                    if self.r#ref.is_some() {
                                        *r#ref = self.r#ref.clone();
                                    }
                                }
                                break;
                            }
                            _ => {}
                        }
                    }

                    if !found {
                        e.get_mut().idls.push(idl);
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(Entry {
                        protocol: idl.protocol(),
                        filename: PathBuf::from(DEFAULT_FILENAME),
                        idls: vec![idl],
                    });
                }
            }
            Ok(())
        })?;

        std::fs::rename(
            DEFAULT_CONFIG_FILE,
            PathBuf::from("./volo-gen/").join(DEFAULT_CONFIG_FILE),
        )?;

        Ok(())
    }
}
