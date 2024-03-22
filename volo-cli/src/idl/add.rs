use std::{collections::HashMap, path::PathBuf};

use clap::{value_parser, Parser};
use volo_build::{
    model::{Entry, Idl, Service, Source},
    util::{check_and_get_repo_name, create_git_service, strip_slash_prefix},
};

use crate::{command::CliCommand, context::Context};

#[derive(Debug, Parser)]
#[command(arg_required_else_help = true)]
pub struct Add {
    #[arg(
        long = "repo",
        help = "Specify the git repo name for repo.\nExample: cloudwego_volo"
    )]
    pub repo: Option<String>,

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
        help = "Specify the git repo ref(branch) for idl.\nExample: main / $TAG"
    )]
    pub r#ref: Option<String>,

    #[arg(
        short = 'i',
        long = "includes",
        help = "Specify the include dirs for idl.\nIf -g or --git is specified, then this should \
                be the path in the specified git repo."
    )]
    pub includes: Vec<PathBuf>,

    #[arg(
        short = 'f',
        long = "filename",
        help = "Specify the output filename, defaults to 'volo_gen.rs'.",
        default_value = "volo_gen.rs"
    )]
    pub filename: String,

    #[arg(
        value_parser = value_parser!(PathBuf),
        help = "Specify the path for idl.\nIf -g or --git is specified, then this should be the \
                path in the specified git repo.\nExample: \t-g not \
                specified:\t./idl/client.thrift\n\t\t-g specified:\t\t/path/to/idl/client.thrift"
    )]
    pub idl: PathBuf,
}

impl CliCommand for Add {
    fn run(&self, cx: Context) -> anyhow::Result<()> {
        if self.filename.contains('/') || self.filename.contains('\\') {
            eprintln!("filename should not contain '/' or '\\'");
            std::process::exit(1);
        }
        volo_build::util::with_config(|config| {
            let local_service = if self.repo.is_none() && self.git.is_none() {
                Some(Service {
                    idl: Idl {
                        source: Source::Local,
                        path: strip_slash_prefix(self.idl.as_path()),
                        includes: self.includes.clone(),
                    },
                    codegen_option: Default::default(),
                })
            } else {
                None
            };

            let mut has_found_entry = false;
            // iter the entries to find the entry
            for (entry_name, entry) in config.entries.iter_mut() {
                if entry_name != &cx.entry_name {
                    if entry.filename == PathBuf::from(&self.filename) {
                        eprintln!(
                            "The specified filename '{}' already exists in entry '{}'!",
                            self.filename, entry_name
                        );
                        std::process::exit(1);
                    }
                    continue;
                }

                // found the entry
                has_found_entry = true;

                if entry.filename != PathBuf::from(&self.filename) {
                    eprintln!(
                        "The specified filename '{}' doesn't match the current filename '{}' in \
                         the entry '{}'!",
                        self.filename,
                        entry.filename.to_string_lossy(),
                        entry_name
                    );
                    std::process::exit(1);
                }

                let mut has_found_idl = false;
                let mut is_existed_local = false;
                // iter idls to find if the idl is already in the entry
                for s in entry.services.iter() {
                    if s.idl.path != self.idl {
                        continue;
                    }
                    if let Source::Local = s.idl.source {
                        is_existed_local = true;
                    }
                    has_found_idl = true;
                    break;
                }

                // case 1: [new local idl]
                if let Some(local_service) = local_service.as_ref() {
                    // case 1.1: exsited git idl or not exsit
                    if !is_existed_local {
                        entry.services.push(local_service.clone());
                    }
                    // case 1.2: local exsited idl, do nothing
                    break;
                }

                // case 2: [new git idl]
                // check and get the repo name
                let mut new_repo = None;
                let repo_name = check_and_get_repo_name(
                    entry_name,
                    &entry.repos,
                    &self.repo,
                    &self.git,
                    &self.r#ref,
                    &mut new_repo,
                )?;

                // check the exact idl service
                if new_repo.is_none() && has_found_idl {
                    eprintln!(
                        "The specified idl '{}' already exists in entry '{}'!",
                        self.idl.to_string_lossy(),
                        entry_name
                    );
                    std::process::exit(1);
                }

                // create the git idl service
                let git_service = create_git_service(repo_name.clone(), &self.idl, &self.includes);
                entry.services.push(git_service);

                // case 2.1: new repo, else case 2.2: exsited repo
                if let Some(new_repo) = new_repo {
                    entry.repos.insert(repo_name, new_repo.clone());
                }

                break;
            }

            if !has_found_entry {
                let mut new_repo = None;
                let repo_name = check_and_get_repo_name(
                    &cx.entry_name,
                    &HashMap::new(),
                    &self.repo,
                    &self.git,
                    &self.r#ref,
                    &mut new_repo,
                )?;
                let new_service = if let Some(local_service) = local_service.as_ref() {
                    local_service.clone()
                } else {
                    create_git_service(repo_name.clone(), &self.idl, &self.includes)
                };

                config.entries.insert(
                    cx.entry_name.clone(),
                    Entry {
                        protocol: new_service.idl.protocol(),
                        filename: PathBuf::from(&self.filename),
                        repos: if let Some(new_repo) = new_repo {
                            let mut repos = HashMap::with_capacity(1);
                            repos.insert(repo_name, new_repo.clone());
                            repos
                        } else {
                            HashMap::new()
                        },
                        services: vec![new_service],
                        common_option: Default::default(),
                    },
                );
            }
            Ok(())
        })
    }
}
