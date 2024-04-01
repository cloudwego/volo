use clap::Parser;
use volo_build::util::check_and_get_repo_name;

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
    pub git: String,
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
        volo_build::util::with_config(|config| {
            let entry = if config.entries.contains_key(&cx.entry_name) {
                config.entries.get_mut(&cx.entry_name).unwrap()
            } else {
                unreachable!("The specified entry should exist when add new repo.");
            };

            let mut new_repo = None;
            // repo name is valid when the repo and git arg are not conflicted with the entry's
            // repos
            let repo_name = check_and_get_repo_name(
                &cx.entry_name,
                &entry.repos,
                &self.repo,
                &Some(self.git.clone()),
                &self.r#ref,
                &mut new_repo,
            )?;

            if let Some(new_repo) = new_repo {
                entry.repos.insert(repo_name.clone(), new_repo);
            } else {
                // not add the repeated repo
                eprintln!(
                    "The specified repo '{}' has already been added in the entry '{}'",
                    self.git, cx.entry_name
                );
                std::process::exit(1);
            }
            Ok(())
        })
    }
}
