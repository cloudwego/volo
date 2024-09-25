use std::{collections::HashMap, path::PathBuf};

use clap::{value_parser, Parser};
use volo_build::{
    model::{Entry, GitSource, Idl, Service, Source},
    util::{check_and_get_repo_name, create_git_service, detect_protocol},
};

use crate::{command::CliCommand, context::Context};

#[derive(Debug, Parser)]
#[command(arg_required_else_help = true)]
pub struct Add {
    #[arg(
        long,
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
                let local_idl = Idl {
                    source: Source::Local,
                    path: self.idl.clone(),
                    includes: self.includes.clone(),
                };
                // only ensure readable when idl is from local
                local_idl.ensure_readable()?;
                Some(Service {
                    idl: local_idl,
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

                // check the protocol
                if entry.protocol != detect_protocol(self.idl.as_path()) {
                    eprintln!(
                        "The specified idl's protocol is conflicted with the specified entry \
                         '{}', whose protocol is {:?}",
                        entry_name, entry.protocol
                    );
                    std::process::exit(1);
                }

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

                let mut found_idl = None;
                let mut is_existed_local = false;
                // iter idls to find if the idl is already in the entry
                for s in entry.services.iter() {
                    if s.idl.path != self.idl {
                        continue;
                    }
                    if let Source::Local = s.idl.source {
                        is_existed_local = true;
                    }
                    found_idl = Some(&s.idl);
                    break;
                }

                // case 1: [local idl]
                if let Some(local_service) = local_service.as_ref() {
                    // case 1.1: exsited git idl or not exsit
                    if !is_existed_local {
                        entry.services.push(local_service.clone());
                    }
                    // case 1.2: local exsited idl, do nothing
                    break;
                }

                // case 2: [git idl]
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

                // check the existed idl service
                if let Some(idl) = found_idl {
                    if new_repo.is_none() {
                        eprintln!(
                            "The specified idl '{}' already exists in entry '{}'!",
                            self.idl.to_string_lossy(),
                            entry_name
                        );
                        std::process::exit(1);
                    }
                    // check if exsited idl with the same repo name but miss the repo in yml
                    match &idl.source {
                        Source::Git(GitSource { repo }) if *repo == repo_name => {
                            eprintln!(
                                "The specified idl '{}' already exists in entry '{}', but the \
                                 repo is missed, check the yml file and delete the existed idl \
                                 before the execution if the idl is the same, or add the missed \
                                 repo for the existed idl and add the new idl later with \
                                 different repo name",
                                self.idl.to_string_lossy(),
                                entry_name
                            );
                            std::process::exit(1);
                        }
                        _ => {}
                    }
                }

                // create the git idl service
                let git_service = create_git_service(&repo_name, &self.idl, &self.includes);
                entry.services.push(git_service);

                // case 2.1: new repo, else case 2.2: exsited repo
                if let Some(new_repo) = new_repo {
                    entry.repos.insert(repo_name, new_repo.clone());
                }

                break;
            }

            if !has_found_entry {
                let mut repos = HashMap::new();
                let new_service = if let Some(local_service) = local_service.as_ref() {
                    local_service.clone()
                } else {
                    let mut new_repo = None;
                    let repo_name = check_and_get_repo_name(
                        &cx.entry_name,
                        &HashMap::new(),
                        &self.repo,
                        &self.git,
                        &self.r#ref,
                        &mut new_repo,
                    )?;
                    repos.insert(
                        repo_name.clone(),
                        new_repo.expect("new entry's git source requires the new repo for idl"),
                    );
                    create_git_service(&repo_name, &self.idl, &self.includes)
                };

                config.entries.insert(
                    cx.entry_name.clone(),
                    Entry {
                        protocol: detect_protocol(new_service.idl.path.as_path()),
                        filename: PathBuf::from(&self.filename),
                        repos,
                        services: vec![new_service],
                        common_option: Default::default(),
                    },
                );
            }
            Ok(())
        })
    }
}
