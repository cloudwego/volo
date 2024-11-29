use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
};

use anyhow::{bail, Context};
use mockall_double::double;
use pilota_build::middle::context::Mode;
use pilota_build::Symbol;
use serde::de::Error;
use volo::FastStr;

use crate::model::{GitSource, Idl, IdlProtocol, Repo, Service, SingleConfig, Source};

pub static DEFAULT_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    std::path::Path::new(
        &std::env::var("OUT_DIR")
            .expect("OUT_DIR is not set, maybe you are calling volo-build outside build.rs?"),
    )
    .join("idl")
});

pub const DEFAULT_CONFIG_FILE: &str = "volo.yml";

pub fn open_config_file<P: AsRef<Path>>(conf_file_name: P) -> std::io::Result<File> {
    ensure_file(conf_file_name.as_ref())
}

pub fn ensure_cache_path() -> std::io::Result<()> {
    ensure_path(&DEFAULT_DIR)
}

pub fn read_config_from_file(f: &mut File) -> Result<SingleConfig, serde_yaml::Error> {
    match f.metadata() {
        Ok(metadata) => {
            if metadata.len() == 0 {
                Ok(SingleConfig::new())
            } else {
                let mut s = String::with_capacity(metadata.len() as usize);
                f.read_to_string(&mut s).map_err(|e| {
                    serde_yaml::Error::custom(format!("failed to read config file, err: {}", e))
                })?;
                match serde_yaml::from_str(s.as_str()) {
                    Ok(config) => Ok(config),
                    Err(e) => {
                        // try to unmarshal by the old format
                        if serde_yaml::from_str::<'_, crate::legacy::model::Config>(&s).is_ok() {
                            Err(serde_yaml::Error::custom(
                                "the config file is in legacy format, please migrate it to the \
                                 new format first, refer: https://www.cloudwego.io/docs/volo/guide/config/",
                            ))
                        } else {
                            Err(e)
                        }
                    }
                }
            }
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(SingleConfig::new())
            } else {
                Err(serde_yaml::Error::custom(format!(
                    "failed to read config file, err: {}",
                    e
                )))
            }
        }
    }
}

pub fn ensure_path(s: &Path) -> std::io::Result<()> {
    fs::create_dir_all(s)
}

pub fn ensure_file(filename: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(filename)
}

/// Pull the minimal, expected .thrift files from a git repository.
pub fn download_files_from_git(task: Task) -> anyhow::Result<()> {
    ensure_path(&task.dir)?;

    git_archive(&task.repo, &task.lock, &task.dir)?;

    Ok(())
}

pub fn download_repo(repo: &Repo, target_dir: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
    let dir = target_dir.as_ref().join(get_git_path(repo.url.as_str())?);

    // check if the repo is already downloaded
    let lock_path = dir.join(repo.lock.as_str());
    if dir.exists() && lock_path.exists() {
        return Ok(dir);
    }

    let task = Task::new(
        vec![],
        dir.clone(),
        repo.url.to_string(),
        repo.lock.to_string(),
    );
    download_files_from_git(task).with_context(|| format!("download repo {}", repo.url))?;

    // write lock file
    File::create(lock_path.clone())
        .with_context(|| format!("couldn't write to lock file: {:?}", lock_path.display()))?;

    Ok(dir)
}

fn run_command(command: &mut Command) -> anyhow::Result<()> {
    command.status().map_err(anyhow::Error::from).and_then(|s| {
        if s.success() {
            Ok(())
        } else {
            bail!("run {:?} failed, exit status: {:?}", command, s)
        }
    })
}

pub fn git_archive(repo: &str, revision: &str, dir: &Path) -> anyhow::Result<()> {
    run_command(Command::new("git").arg("init").current_dir(dir))?;
    let _ = run_command(
        Command::new("git")
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg(repo)
            .current_dir(dir),
    );

    run_command(
        Command::new("git")
            .arg("fetch")
            .arg("origin")
            .arg(revision)
            .arg("--depth=1")
            .current_dir(dir),
    )?;

    run_command(
        Command::new("git")
            .arg("reset")
            .arg("--hard")
            .arg(revision)
            .current_dir(dir),
    )?;

    Ok(())
}

pub fn get_git_path(git: &str) -> anyhow::Result<PathBuf> {
    // there may be two type of git here:
    // 1. username@domain:namespace/repo.git
    // 2. https://domain/namespace/repo.git
    let g = git.trim_end_matches(".git");
    let s1 = g.split(':');
    let s11 = s1.clone();
    match s11.count() {
        1 => Ok(PathBuf::from(g)), // doesn't match any of the pattern, we assume it's a local path
        2 => {
            if g.starts_with("https") {
                return Ok(PathBuf::from(g.trim_start_matches("https://"))); // prefixed with https://
            }
            // prefixed with username@
            let s1vec: Vec<&str> = s1.collect();
            let s2: Vec<&str> = s1vec[0].split('@').collect();
            let mut res = String::new();
            if s2.len() == 1 {
                res.push_str(s2[0]);
            } else {
                res.push_str(s2[1]);
            }
            res.push('/');
            res.push_str(s1vec[1]);
            Ok(PathBuf::from(res))
        }
        _ => Err(anyhow::format_err!("git format error: {}", git)),
    }
}

pub fn get_repo_name_by_url(git: &str) -> &str {
    // there may be two type of git here:
    // 1. username@domain:namespace/repo.git
    // 2. https://domain/namespace/repo.git
    let g = git.trim_end_matches(".git");
    g.rsplit_once('/').map(|s| s.1).unwrap_or(g)
}

pub struct Task {
    _files: Vec<String>,
    dir: PathBuf,
    repo: String,
    lock: String,
}

impl Task {
    pub fn new(files: Vec<String>, dir: PathBuf, repo: String, lock: String) -> Task {
        Task {
            _files: files,
            dir,
            repo,
            lock,
        }
    }
}

mod outer {
    use mockall::automock;
    #[automock]
    pub mod git {
        use std::process::Command;

        use anyhow::bail;

        pub fn get_repo_latest_commit_id(repo: &str, r#ref: &str) -> anyhow::Result<String> {
            let commit_list = match Command::new("git")
                .arg("ls-remote")
                .arg(repo)
                .arg(r#ref)
                .output()
            {
                Ok(output) => unsafe { String::from_utf8_unchecked(output.stdout) },
                Err(e) => {
                    bail!("git ls-remote {} {} err:{}", repo, r#ref, e);
                }
            };
            let commit_list: Vec<_> = commit_list
                .split('\n')
                .filter_map(|s| {
                    let v: Vec<_> = s.split('\t').collect();
                    (v.len() == 2).then_some(v)
                })
                .collect();
            match commit_list.len() {
                0 => {
                    bail!(
                        "get latest commit of {}:{} failed, please check the {} of {}",
                        repo,
                        r#ref,
                        r#ref,
                        repo
                    );
                }
                // happy path
                1 => {}
                _ => {
                    let possibilities = commit_list
                        .iter()
                        .map(|x| x[1])
                        .collect::<Vec<_>>()
                        .join("\n");
                    bail!(
                        "get latest commit of {}:{} failed because of multiple refs, please \
                         choose one of: \n{}",
                        repo,
                        r#ref,
                        possibilities,
                    );
                }
            }
            let commit_id = commit_list[0][0];
            Ok(commit_id.into())
        }
    }
}

#[double]
pub use outer::git;

use self::git::get_repo_latest_commit_id;

pub fn with_config<F, R>(func: F) -> anyhow::Result<R>
where
    F: FnOnce(&mut SingleConfig) -> anyhow::Result<R>,
{
    const DOC_HINT: &str = "# Please refer to https://www.cloudwego.io/docs/volo/guide/config/ \
                            for the configuration file format.\n";

    // open config file and read
    let mut f = open_config_file(DEFAULT_CONFIG_FILE).context("open config file")?;
    let mut config = read_config_from_file(&mut f).context("read config file")?;

    let r = func(&mut config)?;

    // write back to config file
    f.rewind()?;
    f.write_all(DOC_HINT.as_bytes())?;
    serde_yaml::to_writer(&mut f, &config).context("write back config file")?;
    let len = f.stream_position()?;
    f.set_len(len)?;

    Ok(r)
}

pub fn git_repo_init(path: &Path) -> anyhow::Result<()> {
    // Check if we are in a git repo and the path to the new package is not an ignored path in that
    // repo.
    //
    // Reference: https://github.com/rust-lang/cargo/blob/0.74.0/src/cargo/util/vcs.rs
    fn in_git_repo(path: &Path) -> bool {
        if let Ok(repo) = git2::Repository::discover(path) {
            // Don't check if the working directory itself is ignored.
            if repo.workdir() == Some(path) {
                true
            } else {
                !repo.is_path_ignored(path).unwrap_or(false)
            }
        } else {
            false
        }
    }

    if !in_git_repo(path) {
        git2::Repository::init(path)?;
    }

    Ok(())
}

pub fn strip_slash_prefix(p: &Path) -> PathBuf {
    match p.strip_prefix("/") {
        Ok(p) => p.to_path_buf(),
        Err(_) => p.to_path_buf(),
    }
}

pub fn init_local_service(idl: impl AsRef<Path>, includes: &[PathBuf]) -> anyhow::Result<Service> {
    let raw_idl = Idl {
        source: Source::Local,
        path: PathBuf::new().join(idl.as_ref()),
        includes: includes.to_vec(),
    };
    // only ensure readable when idl is from local
    raw_idl.ensure_readable()?;

    Ok(Service {
        idl: raw_idl,
        codegen_option: Default::default(),
    })
}

// the yml file will move into the volo-gen directory generated at the init cmd execution path,
// so the path of the idl file and includes should be modified to relative path by add '../' if the
// raw is relative
pub fn modify_local_init_service_path_relative_to_yml(service: &mut Service) {
    if let Source::Local = service.idl.source {
        if service.idl.path.is_relative() {
            service.idl.path = PathBuf::new().join("../").join(service.idl.path.clone());
        }
        service.idl.includes = service
            .idl
            .includes
            .iter()
            .map(|i| {
                if i.is_relative() {
                    PathBuf::new().join("../").join(i.clone())
                } else {
                    i.clone()
                }
            })
            .collect();
    }
}

pub fn init_git_repo(
    repo: &Option<String>,
    git: &str,
    r#ref: &Option<String>,
) -> anyhow::Result<(FastStr, Repo)> {
    let repo_name = FastStr::new(repo.as_deref().unwrap_or_else(|| get_repo_name_by_url(git)));
    let r#ref = r#ref.as_deref().unwrap_or("HEAD");
    let lock = get_repo_latest_commit_id(git, r#ref)?;
    let new_repo = Repo {
        url: FastStr::new(git),
        r#ref: FastStr::new(r#ref),
        lock: lock.into(),
    };
    Ok((repo_name, new_repo))
}

pub fn download_repos_to_target(
    repos: &HashMap<FastStr, Repo>,
    target_dir: impl AsRef<Path>,
) -> anyhow::Result<HashMap<FastStr, PathBuf>> {
    let mut repo_dir_map = HashMap::with_capacity(repos.len());
    for (name, repo) in repos {
        let dir = download_repo(repo, target_dir.as_ref())?;
        repo_dir_map.insert(name.clone(), dir);
    }
    Ok(repo_dir_map)
}

pub fn get_idl_build_path_and_includes(
    idl: &Idl,
    repo_dir_map: &HashMap<FastStr, PathBuf>,
) -> (PathBuf, Vec<PathBuf>) {
    if let Source::Git(GitSource { ref repo }) = idl.source {
        // git should use relative path instead of absolute path
        let dir = repo_dir_map
            .get(repo)
            .expect("git source requires the repo info for idl")
            .clone();
        let path = dir.join(strip_slash_prefix(idl.path.as_path()));
        let mut includes: Vec<PathBuf> = idl.includes.iter().map(|v| dir.join(v.clone())).collect();
        // To resolve absolute path dependencies, go back two levels to the domain level
        if let Some(path) = dir.parent().and_then(|d| d.parent()) {
            includes.push(path.to_path_buf());
        }
        (path, includes)
    } else {
        (idl.path.clone(), idl.includes.clone())
    }
}

#[derive(Default)]
pub struct ServiceBuilder {
    pub path: PathBuf,
    pub includes: Vec<PathBuf>,
    pub touch: Vec<String>,
    pub keep_unknown_fields: bool,
}

pub fn get_service_builders_from_services(
    services: &[Service],
    repo_dir_map: &HashMap<FastStr, PathBuf>,
) -> Vec<ServiceBuilder> {
    services
        .iter()
        .map(|s| {
            let (path, includes) = get_idl_build_path_and_includes(&s.idl, repo_dir_map);
            ServiceBuilder {
                path,
                includes,
                touch: s.codegen_option.touch.clone(),
                keep_unknown_fields: s.codegen_option.keep_unknown_fields,
            }
        })
        .collect()
}

pub fn check_and_get_repo_name(
    entry_name: &String,
    repos: &HashMap<FastStr, Repo>,
    repo: &Option<String>,
    git: &Option<String>,
    r#ref: &Option<String>,
    new_repo: &mut Option<Repo>,
) -> anyhow::Result<FastStr> {
    // valid when one of these meets:
    // 1. the repo is not existed in the entry, that is repo name and url are not existed and the
    //    new repo's url must be provided
    // 2. the repo is existed in the entry, and the url, ref is the same, and the repo name is the
    //    same when provided
    let url_map = {
        let mut map = HashMap::<FastStr, FastStr>::with_capacity(repos.len());
        repos.iter().for_each(|(key, repo)| {
            let _ = map.insert(repo.url.clone(), key.clone());
        });
        map
    };
    let r#ref = FastStr::new(r#ref.as_deref().unwrap_or("HEAD"));
    let repo_name = match (repo.as_ref(), git.as_ref()) {
        (Some(repo_name), Some(git)) => {
            // check repo by repo name index
            let key: FastStr = repo_name.clone().into();
            if repos.contains_key(&key) {
                let repo = repos.get(&key).unwrap();
                if repo.url != git {
                    bail!(
                        "The specified repo '{}' already exists in entry '{}' with different url",
                        key,
                        entry_name,
                    );
                } else if repo.r#ref != r#ref {
                    bail!(
                        "The specified repo '{}' already exists in entry '{}' with different ref  \
                         '{}'",
                        key,
                        entry_name,
                        r#ref
                    );
                }
            } else {
                // check repo by git url index
                if url_map.contains_key(&FastStr::new(git)) {
                    bail!(
                        "The specified repo '{}' is indexed by the existed repo name '{}' in \
                         entry '{}', please use the existed one",
                        git,
                        url_map.get(&FastStr::new(git)).unwrap(),
                        entry_name
                    );
                }
                let lock = get_repo_latest_commit_id(git, &r#ref)?.into();
                let _ = new_repo.insert(Repo {
                    url: git.clone().into(),
                    r#ref: r#ref.clone(),
                    lock,
                });
            }
            key.clone()
        }
        (Some(repo_name), _) => {
            // the repo should exist in the entry
            let key: FastStr = repo_name.clone().into();
            if !repos.contains_key(&key) {
                bail!(
                    "The specified repo index '{}' not exists in entry '{}', please use the \
                     existed repo name or specify the git url for the new repo",
                    key,
                    entry_name
                );
            }
            key.clone()
        }
        (_, Some(git)) => {
            let key = FastStr::new(git);
            if url_map.contains_key(&key) {
                // check repo by git url index
                let repo = url_map.get(&key).unwrap();
                let existed_ref = &repos
                    .get(repo)
                    .expect("the repo index should exist for the git index map")
                    .r#ref;
                if existed_ref.clone() != r#ref {
                    bail!(
                        "The specified repo '{}' already exists in entry '{}' with different ref \
                         '{}', please check and use the correct one.",
                        key,
                        entry_name,
                        existed_ref
                    );
                }
                repo.clone()
            } else {
                let name = FastStr::new(get_repo_name_by_url(git));
                if repos.contains_key(&name) {
                    bail!(
                        "The specified repo '{git}' with the default index '{name}' generated by \
                         git url is conflicted with the existed one in entry '{entry_name}'",
                    )
                }
                // create a new repo by the git url
                let lock = get_repo_latest_commit_id(git, &r#ref)?.into();
                let _ = new_repo.insert(Repo {
                    url: git.clone().into(),
                    r#ref: r#ref.clone(),
                    lock,
                });
                name
            }
        }
        _ => {
            bail!("Either the repo or the git arg should be specified!")
        }
    };
    Ok(repo_name)
}

pub fn create_git_service(repo: &FastStr, idl_path: &Path, includes: &[PathBuf]) -> Service {
    Service {
        idl: Idl {
            source: Source::Git(GitSource { repo: repo.clone() }),
            path: strip_slash_prefix(idl_path),
            includes: includes.to_vec(),
        },
        codegen_option: Default::default(),
    }
}

pub fn detect_protocol<P: AsRef<Path>>(path: P) -> IdlProtocol {
    let path = path.as_ref();
    match path.extension().and_then(|v| v.to_str()) {
        Some("thrift") => IdlProtocol::Thrift,
        Some("proto") => IdlProtocol::Protobuf,
        _ => {
            eprintln!("invalid file ext {:?}", path);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::{tempdir, NamedTempFile};

    use super::*;

    #[test]
    fn test_ensure_path() {
        // Test case 1: directory already exists
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing_dir");
        fs::create_dir_all(&path).unwrap();
        assert!(ensure_path(&path).is_ok());

        // Test case 2: directory does not exist
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new_dir");
        assert!(ensure_path(&path).is_ok());
        assert!(fs::metadata(&path).unwrap().is_dir());
    }

    #[test]
    fn test_ensure_file() {
        // Test case 1: File does not exist
        let result = tempdir().unwrap();
        let binding = result.path().join("non_existing_file.txt");
        let filename1 = binding.as_path();
        match ensure_file(filename1) {
            Ok(file) => {
                assert!(file.metadata().is_ok());
            }
            Err(err) => {
                eprintln!("Error: {}", err);
                panic!("Failed to create new file");
            }
        }

        // Test case 2: File already exists
        let file1 = NamedTempFile::new().unwrap();
        let filename2 = file1.path();
        match ensure_file(filename2) {
            Ok(file) => {
                assert!(file.metadata().is_ok());
            }
            Err(err) => {
                eprintln!("Error: {}", err);
                panic!("Failed to append to existing file");
            }
        }
    }

    #[test]
    fn test_get_repo_name_by_url() {
        let url = "username@domain:namespace/repo.git";
        assert_eq!(get_repo_name_by_url(url), "repo");
        let url = "https://domain/namespace/repo.git";
        assert_eq!(get_repo_name_by_url(url), "repo");
    }

    #[test]
    fn test_get_git_path() {
        let git = "username@domain:namespace/repo.git";
        assert_eq!(
            get_git_path(git).unwrap(),
            PathBuf::from("domain/namespace/repo")
        );
        let git = "https://domain/namespace/repo.git";
        assert_eq!(
            get_git_path(git).unwrap(),
            PathBuf::from("domain/namespace/repo")
        );
        let git = "../path/to/repo";
        assert_eq!(get_git_path(git).unwrap(), PathBuf::from("../path/to/repo"));
    }

    #[test]
    fn test_get_idl_build_path_and_includes() {
        let idl = Idl {
            source: Source::Local,
            path: PathBuf::from("../idl/test.thrift"),
            includes: vec![PathBuf::from("../idl")],
        };
        let repo_dir_map = HashMap::new();
        assert_eq!(
            get_idl_build_path_and_includes(&idl, &repo_dir_map),
            (
                PathBuf::from("../idl/test.thrift"),
                vec![PathBuf::from("../idl")]
            )
        );

        let idl = Idl {
            source: Source::Git(GitSource {
                repo: "test".into(),
            }),
            path: PathBuf::from("idl/test.thrift"),
            includes: vec![PathBuf::from("idl")],
        };
        let mut repo_dir_map = HashMap::new();
        repo_dir_map.insert("test".into(), PathBuf::from("repo"));
        assert_eq!(
            get_idl_build_path_and_includes(&idl, &repo_dir_map),
            (
                PathBuf::from("repo/idl/test.thrift"),
                vec![PathBuf::from("repo/idl")]
            )
        );
    }

    #[test]
    fn test_get_service_builders_from_services() {
        let idl = Idl {
            source: Source::Local,
            path: PathBuf::from("../idl/test.thrift"),
            includes: vec![PathBuf::from("../idl")],
        };
        let service = Service {
            idl: idl.clone(),
            codegen_option: Default::default(),
        };
        let services = vec![service];
        let repo_dir_map = HashMap::new();
        let builders = get_service_builders_from_services(&services, &repo_dir_map);
        assert_eq!(builders.len(), 1);
        assert_eq!(builders[0].path, idl.path);
    }

    #[test]
    fn test_check_and_get_repo_name() {
        let mut repos = HashMap::new();
        let repo = Repo {
            url: "https://domain/namespace/repo.git".into(),
            r#ref: "main".into(),
            lock: "123456".into(),
        };
        repos.insert("test".into(), repo);
        let mut new_repo: Option<Repo> = None;

        // case 1: exist repo
        // case 1.1 correct with provided name, url, ref
        let name_result = check_and_get_repo_name(
            &"test_entry".into(),
            &repos,
            &Some("test".into()),
            &Some("https://domain/namespace/repo.git".into()),
            &Some("main".into()),
            &mut new_repo,
        );
        assert!(name_result.is_ok());
        assert_eq!(name_result.unwrap(), "test");
        assert!(new_repo.is_none());

        // case 1.2 correct with provided url, ref
        new_repo = None;
        let name_result = check_and_get_repo_name(
            &"test_entry".into(),
            &repos,
            &None,
            &Some("https://domain/namespace/repo.git".into()),
            &Some("main".into()),
            &mut new_repo,
        );
        assert!(name_result.is_ok());
        assert_eq!(name_result.unwrap(), "test");
        assert!(new_repo.is_none());

        // case 1.3 incorrect with provided name not equal to existed name
        new_repo = None;
        let name = check_and_get_repo_name(
            &"test_entry".into(),
            &repos,
            &Some("conflict".into()),
            &Some("https://domain/namespace/repo.git".into()),
            &Some("main".into()),
            &mut new_repo,
        );
        assert!(new_repo.is_none());
        assert!(name.is_err());
        assert_eq!(
            name.unwrap_err().to_string(),
            "The specified repo 'https://domain/namespace/repo.git' is indexed by the existed \
             repo name 'test' in entry 'test_entry', please use the existed one"
        );

        // case 1.4 incorrect with provided name not exist
        new_repo = None;
        let name = check_and_get_repo_name(
            &"test_entry".into(),
            &repos,
            &Some("not_exist".into()),
            &None,
            &None,
            &mut new_repo,
        );
        assert!(new_repo.is_none());
        assert!(name.is_err());
        assert_eq!(
            name.unwrap_err().to_string(),
            "The specified repo index 'not_exist' not exists in entry 'test_entry', please use \
             the existed repo name or specify the git url for the new repo"
        );

        // case 2 : new repo
        // case 2.1 correct with provided name, url, ref
        new_repo = None;
        // mock remote git
        let ctx = git::get_repo_latest_commit_id_context();
        ctx.expect().return_once(|_, _| Ok("123456".into()));
        let name_result = check_and_get_repo_name(
            &"test_entry".into(),
            &repos,
            &Some("new".into()),
            &Some("https://domain/namespace/repo2.git".into()),
            &Some("main".into()),
            &mut new_repo,
        );
        ctx.checkpoint();
        assert!(name_result.is_ok());
        assert_eq!(name_result.unwrap(), "new");
        assert!(new_repo.is_some());
        let Repo { url, r#ref, lock } = new_repo.unwrap();
        assert_eq!(url, "https://domain/namespace/repo2.git");
        assert_eq!(r#ref, "main");
        assert_eq!(lock, "123456");

        // case 2.2 correct with provided url, ref
        new_repo = None;
        // mock remote git
        let ctx = git::get_repo_latest_commit_id_context();
        ctx.expect().return_once(|_, _| Ok("123456".into()));
        let name_result = check_and_get_repo_name(
            &"test_entry".into(),
            &repos,
            &None,
            &Some("https://domain/namespace/repo2.git".into()),
            &Some("main".into()),
            &mut new_repo,
        );
        ctx.checkpoint();
        assert!(name_result.is_ok());
        assert_eq!(name_result.unwrap(), "repo2");
        assert!(new_repo.is_some());
        let Repo { url, r#ref, lock } = new_repo.unwrap();
        assert_eq!(url, "https://domain/namespace/repo2.git");
        assert_eq!(r#ref, "main");
        assert_eq!(lock, "123456");

        // case 2.3 incorrect with provided name conflict
        new_repo = None;
        let name_result = check_and_get_repo_name(
            &"test_entry".into(),
            &repos,
            &Some("test".into()),
            &Some("https://domain/namespace/repo2.git".into()),
            &Some("main".into()),
            &mut new_repo,
        );
        assert!(name_result.is_err());
        assert!(new_repo.is_none());
        assert_eq!(
            name_result.unwrap_err().to_string(),
            "The specified repo 'test' already exists in entry 'test_entry' with different url"
        );
    }
}

pub(crate) fn write_item(stream: &mut String, base_dir: &Path, name: String, impl_str: String) {
    let path_buf = base_dir.join(&name);
    let path = path_buf.as_path();
    write_file(path, impl_str);
    stream.push_str(format!("include!(\"{}\");", &name).as_str());
}

pub(crate) fn write_file(path: &Path, stream: String) {
    let mut file_writer = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    file_writer.write_all(stream.as_bytes()).unwrap();
    file_writer.flush().unwrap();
    pilota_build::fmt::fmt_file(path);
}

pub(crate) fn get_base_dir(mode: &Mode, def_id: Option<&usize>, path: &[Symbol]) -> PathBuf {
    // Locate directory based on the full item path
    let base_dir = match mode {
        // In a workspace mode, the base directory is next to the `.rs` file for the service
        Mode::Workspace(info) => {
            let mut dir = info.dir.clone();
            if path.is_empty() {
                dir
            } else {
                dir.push(path[0].0.as_str());
                if path.len() > 1 {
                    dir.push("src");
                    for segment in path.iter().skip(1) {
                        dir.push(Path::new(segment.0.as_str()));
                    }
                }
                dir
            }
        }
        // In single file mode, the files directory is the root
        // The base directory path is the root + the item path
        Mode::SingleFile { file_path } => {
            let mut dir = file_path.clone();
            dir.pop();
            for segment in path {
                dir.push(Path::new(segment.0.as_str()));
            }
            dir
        }
    };

    let base_dir = if let Some(suffix) = def_id {
        format!("{}_{suffix}", base_dir.display())
    } else {
        base_dir.display().to_string()
    };
    let base_dir = Path::new(&base_dir);
    base_dir.to_path_buf()
}
