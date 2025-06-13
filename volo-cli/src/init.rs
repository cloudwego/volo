use std::{collections::HashMap, fs::create_dir_all, fs::remove_file, path::PathBuf, process::Command};

use clap::{value_parser, Parser};
use volo_build::{
    config_builder::InitBuilder,
    model::{Entry, DEFAULT_FILENAME},
    util::{
        create_git_service, detect_protocol, git_repo_init, init_git_repo, init_local_service,
        modify_local_init_service_path_relative_to_yml, DEFAULT_CONFIG_FILE,
    },
};

use crate::command::CliCommand;

#[derive(Parser, Debug)]
#[command(about = "init your thrift or grpc project")]
pub struct Init {
    #[arg(help = "The name of project")]
    pub name: String,
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
    pub git: Option<String>,
    #[arg(
        short = 'r',
        long = "ref",
        requires = "git",
        help = "Specify the git repo ref(branch) for idl.\nExample: main / $TAG"
    )]
    pub r#ref: Option<String>,
    #[arg(
        short = 'i',
        long = "includes",
        help = "Specify the include dirs for idl.\nIf -g or --git is specified, then this should \
                be the path in the specified git repo."
    )]
    pub includes: Vec<PathBuf>,
    #[arg(
        value_parser = value_parser!(PathBuf),
        help = "Specify the path for idl.\nIf -g or --git is specified, then this should be the \
                path in the specified git repo.\nExample: \t-g not \
                specified:\t./idl/server.thrift\n\t\t-g specified:\t\t/path/to/idl/server.thrift"
    )]
    pub idl: PathBuf,
}

impl Init {
    pub fn is_grpc_project(&self) -> bool {
        if let Some(ext) = self.idl.extension() {
            ext == "proto"
        } else {
            false
        }
    }

    fn init_gen(&self, config_entry: Entry) -> anyhow::Result<(String, String)> {
        InitBuilder::new(config_entry).init()
    }

    fn copy_grpc_template(&self, config_entry: Entry) -> anyhow::Result<()> {
        let (service_global_name, methods) = self.init_gen(config_entry)?;

        let name = self.name.replace(['.', '-'], "_");
        let cwd = std::env::current_dir()?;
        let folder = cwd.as_path();

        // root dirs
        crate::templates_to_target_file!(folder, "templates/grpc/gitignore", ".gitignore");
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/cargo_toml",
            "Cargo.toml",
            name = &name
        );

        // src dirs
        create_dir_all(folder.join("src/bin"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/src/bin/server_rs",
            "src/bin/server.rs",
            name = &name,
            service_global_name = &service_global_name,
        );
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/src/lib_rs",
            "src/lib.rs",
            service_global_name = &service_global_name,
            methods = &methods,
        );

        // volo-gen dirs
        create_dir_all(folder.join("volo-gen/src"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/volo-gen/build_rs",
            "volo-gen/build.rs"
        );
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/volo-gen/cargo_toml",
            "volo-gen/Cargo.toml",
        );
        crate::templates_to_target_file!(
            folder,
            "templates/grpc/volo-gen/src/lib_rs",
            "volo-gen/src/lib.rs",
        );

        Ok(())
    }

    fn copy_thrift_template(&self, config_entry: Entry) -> anyhow::Result<()> {
        let (service_global_name, methods) = self.init_gen(config_entry)?;

        let name = self.name.replace(['.', '-'], "_");
        let cwd = std::env::current_dir()?;
        let folder = cwd.as_path();

        // root dirs
        crate::templates_to_target_file!(folder, "templates/thrift/gitignore", ".gitignore");
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/cargo_toml",
            "Cargo.toml",
            name = &name
        );

        // src dirs
        create_dir_all(folder.join("src/bin"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/src/bin/server_rs",
            "src/bin/server.rs",
            name = &name,
            service_global_name = &service_global_name,
        );
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/src/lib_rs",
            "src/lib.rs",
            service_global_name = &service_global_name,
            methods = &methods,
        );

        // volo-gen dirs
        create_dir_all(folder.join("volo-gen/src"))?;
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/volo-gen/build_rs",
            "volo-gen/build.rs"
        );
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/volo-gen/cargo_toml",
            "volo-gen/Cargo.toml",
        );
        crate::templates_to_target_file!(
            folder,
            "templates/thrift/volo-gen/src/lib_rs",
            "volo-gen/src/lib.rs",
        );

        Ok(())
    }

    fn clear_empty_config_files(&self) -> anyhow::Result<()> {
        let paths = [
            PathBuf::from(DEFAULT_CONFIG_FILE),
            PathBuf::from("./volo-gen/").join(DEFAULT_CONFIG_FILE),
        ];

        for path in paths {
            if let Ok(metadata) = std::fs::metadata(&path) {
                if metadata.len() == 0 {
                    remove_file(&path).map(|_| eprintln!("Empty {DEFAULT_CONFIG_FILE} removed"))
                        .map_err(|err| anyhow::anyhow!("Failed to delete {}: {}", DEFAULT_CONFIG_FILE, err))?;
                    break;
                }
            }
        }

        Ok(())       
    }
}

impl CliCommand for Init {
    fn run(&self, cx: crate::context::Context) -> anyhow::Result<()> {
        self.clear_empty_config_files()?;

        if std::fs::metadata(DEFAULT_CONFIG_FILE).is_ok()
            || std::fs::metadata(PathBuf::from("./volo-gen/").join(DEFAULT_CONFIG_FILE)).is_ok()
        {
            eprintln!("{DEFAULT_CONFIG_FILE} already exists, the initialization is not allowed!");
            std::process::exit(1);
        }

        volo_build::util::with_config(|config| {
            let mut repos = HashMap::new();
            let mut service = if let Some(git) = self.git.as_ref() {
                let (repo_name, repo) = init_git_repo(&self.repo, git, &self.r#ref)?;
                repos.insert(repo_name.clone(), repo);
                create_git_service(&repo_name, self.idl.as_path(), &self.includes)
            } else {
                init_local_service(self.idl.as_path(), &self.includes)?
            };

            let mut entry = Entry {
                filename: PathBuf::from(DEFAULT_FILENAME),
                protocol: detect_protocol(service.idl.path.as_path()),
                repos,
                services: vec![service.clone()],
                common_option: Default::default(),
            };

            if self.is_grpc_project() {
                self.copy_grpc_template(entry.clone())?;
            } else {
                self.copy_thrift_template(entry.clone())?;
            }

            modify_local_init_service_path_relative_to_yml(&mut service);
            entry.services = vec![service];

            config.entries.insert(cx.entry_name.clone(), entry);

            Ok(())
        })?;

        std::fs::rename(
            DEFAULT_CONFIG_FILE,
            PathBuf::from("./volo-gen/").join(DEFAULT_CONFIG_FILE),
        )?;

        let _ = Command::new("cargo").arg("fmt").arg("--all").output()?;

        let cwd = std::env::current_dir()?;
        git_repo_init(&cwd)?;

        Ok(())
    }
}
