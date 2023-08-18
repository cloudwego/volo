use anyhow::Result;
use clap::Parser;
use colored::*;
use log::{debug, error};
use update_informer::{registry, Check};
use volo_cli::model;

fn main() -> Result<()> {
    // set default log level if not set
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "WARN");
    }
    pretty_env_logger::init();

    let cmd = model::RootCommand::parse();
    // set log level according to verbose
    match cmd.verbose {
        0 => log::set_max_level(log::LevelFilter::Info),
        1 => log::set_max_level(log::LevelFilter::Debug),
        _ => log::set_max_level(log::LevelFilter::Trace),
    }
    debug!("Command parse result: {:?}", cmd);
    let res = cmd.run();
    if let Err(e) = res.as_ref() {
        error!("{}", e);
    }

    // detech new version and notify the user
    let pkg_name = env!("CARGO_PKG_NAME");
    let current_version = env!("CARGO_PKG_VERSION");

    let informer = update_informer::new(registry::Crates, pkg_name, current_version);
    if let Some(version) = informer.check_version().ok().flatten() {
        let outdated_msg = format!(
            "A new release of {pkg_name} is available: v{current_version} -> {new_version}",
            pkg_name = pkg_name.italic().cyan(),
            current_version = current_version,
            new_version = version.to_string().green()
        );

        let update_command = format!("cargo install {pkg_name}").yellow();

        let update_msg =
            format!("You can use '{update_command}' to update to the latest version.",);

        println!("\n{outdated_msg}\n{update_msg}");
    }

    res
}
