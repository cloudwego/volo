use std::{collections::HashMap, fs::create_dir_all, path::PathBuf, process::Command};

use clap::{value_parser, Parser};
use faststr::FastStr;
use volo_build::{
    config_builder::InitBuilder,
    model::{Entry, GitSource, Idl, Repo, Service, Source, DEFAULT_FILENAME},
    util::{
        get_repo_latest_commit_id, get_repo_name_by_url, git_repo_init, strip_slash_prefix,
        DEFAULT_CONFIG_FILE,
    },
};

use crate::command::CliCommand;

#[derive(Parser, Debug)]
#[command(about = "init your thrift or grpc project")]
pub struct Init {
    #[arg(help = "The name of project")]
    pub name: String,
    #[arg(help = "The name of repo")]
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
    pub includes: Option<Vec<PathBuf>>,
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

    fn init_gen(
        &self,
        entry_name: String,
        config_entry: Entry,
    ) -> anyhow::Result<(String, String)> {
        InitBuilder::new(entry_name, config_entry).init()
    }

    fn copy_grpc_template(&self, entry_name: String, config_entry: Entry) -> anyhow::Result<()> {
        std::env::set_var("OUT_DIR", "/tmp/idl");
        let (service_global_name, methods) = self.init_gen(entry_name, config_entry)?;

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

    fn copy_thrift_template(&self, entry_name: String, config_entry: Entry) -> anyhow::Result<()> {
        std::env::set_var("OUT_DIR", "/tmp/idl");
        let (service_global_name, methods) = self.init_gen(entry_name, config_entry)?;

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
}

impl CliCommand for Init {
    fn run(&self, cx: crate::context::Context) -> anyhow::Result<()> {
        if std::fs::metadata(DEFAULT_CONFIG_FILE).is_ok()
            || std::fs::metadata(PathBuf::from("./volo-gen/").join(DEFAULT_CONFIG_FILE)).is_ok()
        {
            eprintln!("volo.yml already exists, the initialization is not allowed!");
            std::process::exit(1);
        }

        volo_build::util::with_config(|_config| {
            let mut idl = Idl::new();
            idl.includes.clone_from(&self.includes);
            let mut repo = None;

            // Handling Git-Based Template Creation
            if let Some(git) = self.git.as_ref() {
                let repo_name = FastStr::new(
                    self.repo
                        .as_deref()
                        .unwrap_or_else(|| get_repo_name_by_url(git)),
                );
                let r#ref = self.r#ref.as_deref().unwrap_or("HEAD");
                let lock = get_repo_latest_commit_id(git, r#ref)?;
                let new_repo = Repo {
                    url: git.clone().into(),
                    r#ref: FastStr::new(r#ref),
                    lock: lock.into(),
                };
                idl.source = Source::Git(GitSource {
                    repo_name: repo_name.clone(),
                });
                repo = Some(new_repo);
            }

            if self.git.is_some() {
                idl.path = strip_slash_prefix(&self.idl);
            } else {
                idl.path.clone_from(&self.idl);
                // only ensure readable when idl is from local
                idl.ensure_readable()?;
            }

            let mut entry = Entry {
                filename: PathBuf::from(DEFAULT_FILENAME),
                protocol: idl.protocol(),
                repos: if let Some(repo) = repo {
                    let mut repos = HashMap::with_capacity(1);
                    let repo_name = if let Source::Git(GitSource { repo_name }) = &idl.source {
                        repo_name.clone()
                    } else {
                        unreachable!("git service should have the git source")
                    };
                    repos.insert(repo_name, repo.clone());
                    repos
                } else {
                    HashMap::new()
                },
                services: vec![Service {
                    idl: idl.clone(),
                    codegen_option: Default::default(),
                }],
                common_option: Default::default(),
            };

            if self.is_grpc_project() {
                self.copy_grpc_template(cx.entry_name.clone(), entry.clone())?;
            } else {
                self.copy_thrift_template(cx.entry_name.clone(), entry.clone())?;
            }

            if self.git.as_ref().is_none() {
                // we will move volo.yml to volo-gen, so we need to add .. to includes and idl path
                if let Some(service) = entry.services.get_mut(0) {
                    if let Some(includes) = &mut service.idl.includes {
                        for i in includes {
                            if i.is_absolute() {
                                continue;
                            }
                            *i = PathBuf::new().join("../").join(i.clone());
                        }
                    }
                    if !idl.path.is_absolute() {
                        idl.path = PathBuf::new().join("../").join(self.idl.clone());
                    }
                }
            }

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
