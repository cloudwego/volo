use clap::Parser;
mod add;
mod update;

use volo_build::model::DEFAULT_ENTRY_NAME;

use self::{add::Add, update::Update};
use crate::{command::CliCommand, context::Context};

define_commands!(RepoCommands { Update, Add });

#[derive(Parser, Debug)]
#[command(about = "manage your repo", arg_required_else_help = true)]
pub struct Repo {
    #[command(subcommand)]
    subcmd: RepoCommands,
    #[arg(
        short = 'n',
        long = "entry-name",
        help = "The entry name, defaults to 'default'.",
        global = true,
        default_value = DEFAULT_ENTRY_NAME
    )]
    pub entry_name: String,
}

impl CliCommand for Repo {
    fn run(&self, cx: Context) -> anyhow::Result<()> {
        self.subcmd.run(cx)
    }
}
