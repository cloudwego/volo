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
        if self.repos.is_empty() {
            eprintln!("repos should not be empty");
            std::process::exit(1);
        }

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

            // check if the git exists in the config
            self.repos.iter().for_each(|g| {
                if !entry.repos.contains_key(&FastStr::new(g)) {
                    eprintln!("git repo {g} not exists in config");
                } else {
                    let r = entry.repos.get_mut(&FastStr::new(g)).unwrap().update();
                    if r.is_err() {
                        eprintln!("update git repo {g} failed");
                    }
                }
            });

            Ok(())
        })
    }
}
