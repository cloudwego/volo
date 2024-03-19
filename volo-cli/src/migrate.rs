use std::{collections::HashMap, fs::create_dir_all, path::PathBuf, process::Command};

use anyhow::anyhow;
use clap::Parser;
use faststr::FastStr;
use volo_build::{
    config_builder::InitBuilder,
    legacy::{self, util::open_config_file},
    model::{
        CodegenOption, CommonOption, Entry, GitSource, Idl, IdlProtocol, Repo, Service, Source,
        DEFAULT_FILENAME,
    },
    util::{
        get_repo_latest_commit_id, get_repo_name_by_url, git_repo_init, strip_slash_prefix,
        DEFAULT_CONFIG_FILE,
    },
};

use crate::command::CliCommand;

#[derive(Parser, Debug)]
#[command(about = "migrate your config from old version")]
pub struct Migrate {}

impl CliCommand for Migrate {
    fn run(&self, cx: crate::context::Context) -> anyhow::Result<()> {
        let path = if std::fs::metadata(DEFAULT_CONFIG_FILE).is_ok() {
            PathBuf::from(DEFAULT_CONFIG_FILE)
        } else {
            return Err(anyhow!("volo.yml not found"));
        };
        let f = open_config_file(path.as_path())?;
        let old_config = legacy::util::read_config_from_file(&f)?;
        drop(f);
        let backup_path = PathBuf::from("volo.yml.bak");
        std::fs::rename(path.as_path(), backup_path.as_path())?;

        volo_build::util::with_config(|config| {
            config.entries = HashMap::with_capacity(old_config.entries.len());
            old_config
                .entries
                .iter()
                .for_each(|(entry_name, old_entry)| {
                    let (repos, services) = transfer_from_legacy(&old_entry.idls);
                    let new_entry = Entry {
                        filename: old_entry.filename.clone(),
                        protocol: match old_entry.protocol {
                            legacy::model::IdlProtocol::Thrift => IdlProtocol::Thrift,
                            legacy::model::IdlProtocol::Protobuf => IdlProtocol::Protobuf,
                        },
                        repos,
                        services,
                        common_option: CommonOption {
                            touch_all: old_entry.touch_all,
                            nonstandard_snake_case: old_entry.nonstandard_snake_case,
                            dedups: Vec::new(),
                        },
                    };

                    config.entries.insert(entry_name.clone(), new_entry);
                });
            Ok(())
        })
        .map_err(|e| {
            if let Err(e) = std::fs::rename(backup_path.as_path(), path.as_path()) {
                eprintln!(
                    "failed to restore backup file: {}, please manually rename it to volo.yml \
                     before retry",
                    e
                );
            }
            e
        })?;
        std::fs::remove_file(backup_path)?;
        Ok(())
    }
}

fn transfer_from_legacy(idls: &Vec<legacy::model::Idl>) -> (HashMap<FastStr, Repo>, Vec<Service>) {
    let mut repos = HashMap::new();
    let mut services = Vec::new();
    idls.into_iter().for_each(|idl| {
        let (repo, service) =
            if let legacy::model::Source::Git(legacy::model::GitSource { repo, r#ref, lock }) =
                &idl.source
            {
                let r#ref: FastStr = r#ref.clone().unwrap_or("HEAD".into()).into();
                let lock = lock
                    .clone()
                    .unwrap_or_else(|| {
                        let r = get_repo_latest_commit_id(&repo, &r#ref);
                        if r.is_err() {
                            eprintln!(
                                "failed to get latest commit id for repo: {}, err: {}",
                                repo,
                                r.err().unwrap()
                            );
                            std::process::exit(1);
                        }
                        r.unwrap()
                    })
                    .into();
                let name = FastStr::new(get_repo_name_by_url(&repo));
                let service = Service {
                    idl: Idl {
                        source: Source::Git(GitSource {
                            repo_name: name.clone(),
                        }),
                        includes: idl.includes.clone(),
                        path: idl.path.clone(),
                    },
                    codegen_option: CodegenOption {
                        keep_unknown_fields: idl.keep_unknown_fields,
                        touch: idl.touch.clone(),
                        ..Default::default()
                    },
                };
                (
                    Some(Repo {
                        url: repo.clone().into(),
                        r#ref,
                        lock,
                    }),
                    service,
                )
            } else {
                (
                    None,
                    Service {
                        idl: Idl {
                            source: Source::Local,
                            includes: idl.includes.clone(),
                            path: idl.path.clone(),
                        },
                        codegen_option: CodegenOption {
                            keep_unknown_fields: idl.keep_unknown_fields,
                            touch: idl.touch.clone(),
                            ..Default::default()
                        },
                    },
                )
            };

        if let Some(repo) = repo {
            let repo_name = if let Source::Git(GitSource { repo_name }) = &service.idl.source {
                repo_name.clone()
            } else {
                unreachable!("git service should have the git source")
            };
            repos.insert(repo_name, repo);
        }
        services.push(service);
    });

    (repos, services)
}
