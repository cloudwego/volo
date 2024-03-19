use std::{collections::HashMap, path::PathBuf};

use anyhow::bail;
use clap::{value_parser, Parser};
use faststr::FastStr;
use volo_build::{
    model::{Entry, GitSource, Idl, Repo, Service, Source},
    util::{get_repo_latest_commit_id, get_repo_name_by_url, strip_slash_prefix},
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
    pub includes: Option<Vec<PathBuf>>,

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
                    match s.idl.source {
                        Source::Local => is_existed_local = true,
                        _ => {}
                    };
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
                // create the git idl service
                let mut new_repo = None;
                let git_service = self.create_git_service(
                    entry_name,
                    &mut new_repo,
                    &entry.repos,
                    has_found_idl,
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

                // case 2.1: new repo, else case 2.2: exsited repo
                if let Some(new_repo) = new_repo {
                    let repo_name =
                        if let Source::Git(GitSource { repo_name }) = &git_service.idl.source {
                            repo_name.clone()
                        } else {
                            unreachable!("git service should have the git source")
                        };
                    entry.repos.insert(repo_name, new_repo.clone());
                }

                entry.services.push(git_service);
                break;
            }

            if !has_found_entry {
                let mut new_repo = None;
                let new_service = if let Some(local_service) = local_service.as_ref() {
                    local_service.clone()
                } else {
                    self.create_git_service(&cx.entry_name, &mut new_repo, &HashMap::new(), false)?
                };

                config.entries.insert(
                    cx.entry_name.clone(),
                    Entry {
                        protocol: new_service.idl.protocol(),
                        filename: PathBuf::from(&self.filename),
                        repos: if let Some(new_repo) = new_repo {
                            let mut repos = HashMap::new();
                            let repo_name = if let Source::Git(GitSource { repo_name }) =
                                &new_service.idl.source
                            {
                                repo_name.clone()
                            } else {
                                unreachable!("git service should have the git source")
                            };
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

impl Add {
    fn create_git_service(
        &self,
        entry_name: &String,
        new_repo: &mut Option<Repo>,
        repos: &HashMap<FastStr, Repo>,
        has_found_idl: bool,
    ) -> Result<Service, anyhow::Error> {
        // valid when one of these meets:
        // 1. the repo is not existed in the entry, that is repo name and url are not existed and
        //    the url is a must
        // 2. the repo is existed in the entry, and the url, ref is the same, and the idl is not,
        //    and the repo name is the same when provided
        let url_map = {
            let mut map = HashMap::<FastStr, FastStr>::with_capacity(repos.len());
            repos.iter().for_each(|(key, repo)| {
                let _ = map.insert(repo.url.clone(), key.clone());
            });
            map
        };
        let r#ref = FastStr::new(self.r#ref.as_deref().unwrap_or("HEAD"));
        let repo_name = match (self.repo.as_ref(), self.git.as_ref()) {
            (Some(repo_name), Some(git)) => {
                // check repo by repo name index
                let key: FastStr = repo_name.clone().into();
                if repos.contains_key(&key) {
                    let repo = repos.get(&key).unwrap();
                    if repo.url != git {
                        bail!(
                            "The specified repo '{}' already exists in entry '{}' with different \
                             url, maybe use another repo name, like {}",
                            key,
                            entry_name,
                            get_repo_name_by_url(git)
                        );
                    } else if has_found_idl {
                        bail!(
                            "The specified idl '{}' is existed in the entry '{}'",
                            self.idl.to_str().unwrap(),
                            entry_name
                        );
                    } else if repo.r#ref != r#ref {
                        bail!(
                            "The specified repo '{}' already exists in entry '{}' with different \
                             ref  '{}'",
                            key,
                            entry_name,
                            r#ref
                        );
                    }
                } else {
                    // check repo by git url rindex
                    if url_map.contains_key(&FastStr::new(git)) {
                        if has_found_idl {
                            bail!(
                                "The specified idl '{}' is existed in the entry '{}'",
                                self.idl.to_str().unwrap(),
                                entry_name
                            );
                        }
                        bail!(
                            "The specified repo '{}' is indexed by the existed repo name '{}' in \
                             entry '{}', please use the existed repo name",
                            git,
                            url_map.get(&FastStr::new(git)).unwrap(),
                            entry_name
                        );
                    }
                    let lock = get_repo_latest_commit_id(git, &r#ref)?.into();
                    let _ = new_repo.insert(Repo {
                        url: git.clone().into(),
                        r#ref: r#ref.clone(),
                        lock,
                    });
                }
                key.clone()
            }
            (Some(repo_name), _) => {
                // the repo should exist in the entry
                let key: FastStr = repo_name.clone().into();
                if !repos.contains_key(&key) {
                    bail!(
                        "The specified repo index '{}' not exists in entry '{}', please use the \
                         existed repo name or specify the git url for the new repo",
                        key,
                        entry_name
                    );
                }
                key.clone()
            }
            (_, Some(git)) => {
                let key = FastStr::new(git);
                if url_map.contains_key(&key) {
                    // check repo by git url rindex
                    if has_found_idl {
                        bail!(
                            "The specified idl '{}' is existed in the entry '{}'",
                            self.idl.to_str().unwrap(),
                            entry_name
                        );
                    }
                    let repo = url_map.get(&key).unwrap();
                    let existed_ref = &repos
                        .get(repo)
                        .expect("the repo index should exist for the git rindex map")
                        .r#ref;
                    if existed_ref.clone() != r#ref {
                        bail!(
                            "The specified repo '{}' already exists in entry '{}' with different \
                             ref '{}', please check and use the correct one.",
                            key,
                            entry_name,
                            existed_ref
                        );
                    }
                    repo.clone()
                } else {
                    // create a new repo by the git url
                    let name = FastStr::new(get_repo_name_by_url(git));
                    let lock = get_repo_latest_commit_id(git, &r#ref)?.into();
                    let _ = new_repo.insert(Repo {
                        url: git.clone().into(),
                        r#ref: r#ref.clone(),
                        lock,
                    });
                    name
                }
            }
            _ => {
                bail!("The specified repo or git should be specified")
            }
        };

        Ok(Service {
            idl: Idl {
                source: Source::Git(GitSource {
                    repo_name: repo_name.clone(),
                }),
                path: strip_slash_prefix(self.idl.as_path()),
                includes: self.includes.clone(),
            },
            codegen_option: Default::default(),
        })
    }
}
