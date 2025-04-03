use clap::Parser;
use faststr::FastStr;

use crate::{command::CliCommand, context::Context};

#[derive(Parser, Debug)]
#[command(about = "update your repo by repo name, split by ','")]
pub struct Update {
    repos: Vec<String>,
}

impl CliCommand for Update {
    fn run(&self, cx: Context) -> anyhow::Result<()> {
        volo_build::util::with_config(|config| {
            if !config.entries.contains_key(&cx.entry_name) {
                eprintln!("entry {} not found", cx.entry_name);
                std::process::exit(1);
            }

            let entry = match config.entries.get_mut(&cx.entry_name) {
                Some(entry) => entry,
                None => {
                    eprintln!("entry {} not found", cx.entry_name);
                    std::process::exit(1);
                }
            };

            if self.repos.is_empty() && entry.repos.is_empty() {
                eprintln!("no repos found in entry {}", cx.entry_name);
                std::process::exit(1);
            } else if self.repos.is_empty() {
                entry.repos.iter_mut().for_each(|(k, v)| {
                    let r = v.update();
                    if r.is_err() {
                        eprintln!("update git repo {k} failed");
                    }
                });
            }

            // check if the repo exists in the config
            self.repos
                .iter()
                .for_each(|g| match entry.repos.get_mut(&FastStr::new(g)) {
                    Some(r) => {
                        let r = r.update();
                        if r.is_err() {
                            eprintln!("update git repo {g} failed");
                        }
                    }
                    None => {
                        eprintln!("git repo {g} not exists in config");
                    }
                });
            Ok(())
        })
    }
}
