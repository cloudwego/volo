use std::collections::HashSet;

use clap::Parser;
use volo_build::{
    model::{GitSource, Source},
    util::get_repo_latest_commit_id,
};

use crate::{command::CliCommand, context::Context};

#[derive(Parser, Debug)]
#[command(about = "update your idl by git repo, split by ','")]
pub struct Update {
    git: Vec<String>,
}

impl CliCommand for Update {
    fn run(&self, cx: Context) -> anyhow::Result<()> {
        volo_build::util::with_config(|config| {
            if !config.entries.contains_key(&cx.entry_name) {
                eprintln!("entry {} not found", cx.entry_name);
                std::process::exit(1);
            }

            let entry = config.entries.get_mut(&cx.entry_name).unwrap();
            let mut exists = HashSet::new();

            entry.idls.iter().for_each(|idl| {
                if let Source::Git(ref git) = idl.source {
                    exists.insert(git.repo.clone());
                }
            });

            // check if the git exists in the config
            self.git.iter().for_each(|g| {
                if !exists.contains(g) {
                    eprintln!("git repo {g} not exists in config");
                    std::process::exit(1);
                }
            });

            let should_update_gits: Vec<*mut GitSource> = {
                if !self.git.is_empty() {
                    self.git
                        .iter()
                        .filter_map(|repo| {
                            entry.idls.iter_mut().find_map(|config_idl| {
                                if let Source::Git(git_source) = &mut config_idl.source {
                                    if *repo == git_source.repo {
                                        return Some(git_source as *mut _);
                                    }
                                }
                                None
                            })
                        })
                        .collect()
                } else {
                    entry
                        .idls
                        .iter_mut()
                        .filter_map(|idl| {
                            if let Source::Git(git_source) = &mut idl.source {
                                Some(git_source as *mut _)
                            } else {
                                None
                            }
                        })
                        .collect()
                }
            };

            should_update_gits.into_iter().try_for_each(|git_source| {
                let commit_id = unsafe {
                    get_repo_latest_commit_id(
                        &git_source.as_ref().unwrap().repo,
                        git_source
                            .as_ref()
                            .unwrap()
                            .r#ref
                            .as_deref()
                            .unwrap_or("HEAD"),
                    )
                }?;

                unsafe {
                    let _ = git_source.as_mut().unwrap().lock.insert(commit_id);
                }

                Ok::<(), anyhow::Error>(())
            })?;

            Ok(())
        })
    }
}
