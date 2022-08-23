use std::path::PathBuf;

use clap::Parser;
use volo_build::{
    model::{Entry, GitSource, Idl, Source},
    util::get_repo_latest_commit_id,
};

use crate::{command::CliCommand, context::Context};

#[derive(Debug, Parser)]
#[clap(arg_required_else_help = true)]
pub struct Add {
    #[clap(
        short = 'g',
        long = "git",
        help = "Specify the git repo for idl.\nShould be in the format of \
                \"git@domain:path/repo.git\".\nExample: git@github.com:cloudwego/volo.git"
    )]
    pub git: Option<String>,
    #[clap(
        short = 'r',
        long = "ref",
        requires = "git",
        help = "Specify the git repo ref(commit/branch) for idl.\nExample: main / $TAG / \
                $COMMIT_HASH"
    )]
    pub r#ref: Option<String>,

    #[clap(
        short = 'i',
        long = "includes",
        help = "Specify the include dirs for idl.\nIf -g or --git is specified, then this should \
                be the path in the specified git repo."
    )]
    pub includes: Option<Vec<PathBuf>>,

    #[clap(
        short = 'f',
        long = "filename",
        help = "Specify the output filename, defaults to 'volo_gen.rs'.",
        default_value = "volo_gen.rs"
    )]
    pub filename: String,

    #[clap(
        parse(from_os_str),
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
            let new_idl = {
                if let Some(git) = self.git.as_ref() {
                    let lock =
                        get_repo_latest_commit_id(git, self.r#ref.as_deref().unwrap_or("HEAD"))?;
                    Idl {
                        source: Source::Git(GitSource {
                            repo: git.clone(),
                            r#ref: self.r#ref.clone(),
                            lock: Some(lock),
                        }),
                        path: self.idl.clone(),
                        includes: self.includes.clone(),
                    }
                } else {
                    Idl {
                        source: Source::Local,
                        path: self.idl.clone(),
                        includes: self.includes.clone(),
                    }
                }
            };

            let mut has_found_entry = false;

            // iter the entries to find the entry
            for (k, v) in config.entries.iter_mut() {
                if k != &cx.entry_name {
                    if v.filename == PathBuf::from(&self.filename) {
                        eprintln!(
                            "The specified filename '{}' already exists in entry '{}'!",
                            self.filename, k
                        );
                        std::process::exit(1);
                    }
                    continue;
                }

                // found the entry
                has_found_entry = true;

                if v.filename != PathBuf::from(&self.filename) {
                    eprintln!(
                        "The specified filename '{}' doesn't match the current filename '{}' in \
                         the entry '{}'!",
                        self.filename,
                        v.filename.to_string_lossy(),
                        k
                    );
                    std::process::exit(1);
                }

                let mut has_found_idl = false;
                // iter idls to find if the idl is already in the entry
                for idl in v.idls.iter_mut() {
                    if idl.path != self.idl {
                        continue;
                    }
                    match idl.source {
                        Source::Git(ref mut source) if self.git.as_ref() == Some(&source.repo) => {
                            if let Some(r#ref) = self.r#ref.as_ref() {
                                let _ = source.r#ref.insert(r#ref.clone());
                                if let Source::Git(ref git) = new_idl.source {
                                    let _ = source.lock.insert(git.lock.clone().unwrap());
                                }
                            }
                            has_found_idl = true;
                            break;
                        }
                        Source::Local if self.git.is_none() => {
                            has_found_idl = true;
                            break;
                        }
                        _ => {}
                    }
                }

                if !has_found_idl {
                    v.idls.push(new_idl.clone());
                }
            }
            if !has_found_entry {
                config.entries.insert(
                    cx.entry_name.clone(),
                    Entry {
                        protocol: new_idl.protocol(),
                        filename: PathBuf::from(&self.filename),
                        idls: vec![new_idl],
                    },
                );
            }
            Ok(())
        })
    }
}
