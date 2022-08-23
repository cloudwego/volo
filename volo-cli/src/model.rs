use anyhow::Result;
use clap::Parser;
use volo_build::model::DEFAULT_ENTRY_NAME;

use crate::{command::CliCommand, context::Context, idl::Idl, init::Init};

define_commands!(Subcommand { Init, Idl });

#[derive(Parser, Debug)]
#[clap(
    name = "volo",
    author,
    version,
    about,
    rename_all = "kebab-case",
    arg_required_else_help = true,
    propagate_version = true
)]
pub struct RootCommand {
    #[clap(
        short = 'v',
        long = "verbose",
        help = "Turn on the verbose mode.",
        global = true,
        parse(from_occurrences)
    )]
    pub verbose: u16,

    #[clap(
        short = 'n',
        long = "entry-name",
        help = "The entry name, defaults to 'default'.",
        default_value = DEFAULT_ENTRY_NAME
    )]
    pub entry_name: String,

    #[clap(subcommand)]
    subcmd: Subcommand,
}

impl RootCommand {
    pub fn run(self) -> Result<()> {
        let cx = Context {
            entry_name: self.entry_name.clone(),
        };
        self.subcmd.run(cx)
    }
}
