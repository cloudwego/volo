use std::path::PathBuf;

use clap::{value_parser, Parser};
use faststr::FastStr;
use volo_build::{
    model::Repo,
    util::{get_repo_latest_commit_id, get_repo_name_by_url},
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
}

impl CliCommand for Add {
    fn run(&self, cx: Context) -> anyhow::Result<()> {
        if self.git.is_none() {
            unreachable!("git should be specified")
        }
        let git = self.git.as_ref().unwrap();

        let name = FastStr::new(
            self.repo
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or_else(|| get_repo_name_by_url(git)),
        );
        let url = FastStr::new(git);
        let r#ref = FastStr::new(self.r#ref.as_deref().unwrap_or("HEAD"));
        let lock = get_repo_latest_commit_id(&url, &r#ref)?.into();
        let new_repo = Repo { url, r#ref, lock };

        volo_build::util::with_config(|config| {
            let mut has_found_entry = false;
            // iter the entries to find the entry
            for (k, v) in config.entries.iter_mut() {
                if k != &cx.entry_name {
                    continue;
                }

                // found the entry
                has_found_entry = true;

                if v.repos.contains_key(&name) {
                    eprintln!(
                        "The specified repo '{}' already exists in entry '{}'!",
                        name, k
                    );
                    std::process::exit(1);
                }

                v.repos.insert(name.clone(), new_repo.clone());
                break;
            }

            if !has_found_entry {
                unreachable!("entry should be found when add new repo");
            }
            Ok(())
        })
    }
}
