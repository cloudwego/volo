use std::{fs::create_dir_all, process::Command};

use clap::Parser;
use volo_build::util::git_repo_init;

use crate::{command::CliCommand, context::Context};

define_commands!(Subcommand { Init });

#[derive(Parser, Debug)]
#[command(about = "manage your http project")]
pub struct Http {
    #[command(subcommand)]
    subcmd: Subcommand,
}

impl CliCommand for Http {
    fn run(&self, cx: Context) -> anyhow::Result<()> {
        self.subcmd.run(cx)
    }
}

#[derive(Parser, Debug)]
#[command(about = "init your http project")]
pub struct Init {
    #[arg(help = "The name of project")]
    pub name: String,
}

impl Init {
    fn copy_template(&self) -> anyhow::Result<()> {
        let name = self.name.replace(['.', '-'], "_");
        let cwd = std::env::current_dir()?;
        let folder = cwd.as_path();

        // root dirs
        crate::templates_to_target_file!(
            folder,
            "templates/http/rust-toolchain_toml",
            "rust-toolchain.toml"
        );
        crate::templates_to_target_file!(folder, "templates/http/gitignore", ".gitignore");
        crate::templates_to_target_file!(
            folder,
            "templates/http/cargo_toml",
            "Cargo.toml",
            name = &name
        );

        // src dirs
        create_dir_all(folder.join("src/bin"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/http/src/bin/server_rs",
            "src/bin/server.rs",
            name = &name,
        );
        crate::templates_to_target_file!(folder, "templates/http/src/lib_rs", "src/lib.rs",);

        Ok(())
    }
}

impl CliCommand for Init {
    fn run(&self, _: Context) -> anyhow::Result<()> {
        self.copy_template()?;

        let _ = Command::new("cargo").arg("fmt").arg("--all").output()?;

        let cwd = std::env::current_dir()?;
        git_repo_init(&cwd)?;

        Ok(())
    }
}
