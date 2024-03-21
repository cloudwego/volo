use std::{
    collections::HashMap,
    fs::{create_dir_all, File, OpenOptions},
    io::Seek,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, Context};
use lazy_static::lazy_static;
use mockall_double::double;
use serde::de::Error;
use volo::FastStr;

use crate::model::{GitSource, Idl, Repo, Service, SingleConfig, Source};

lazy_static! {
    pub static ref DEFAULT_DIR: PathBuf = std::path::Path::new(
        &std::env::var("OUT_DIR")
            .expect("OUT_DIR is not set, maybe you are calling volo-build outside build.rs?")
    )
    .join("idl");
}

pub const DEFAULT_CONFIG_FILE: &str = "volo.yml";

pub fn open_config_file<P: AsRef<Path>>(conf_file_name: P) -> std::io::Result<File> {
    ensure_file(conf_file_name.as_ref())
}

pub fn ensure_cache_path() -> std::io::Result<()> {
    ensure_path(&DEFAULT_DIR)
}

pub fn read_config_from_file(f: &File) -> Result<SingleConfig, serde_yaml::Error> {
    match f.metadata() {
        Ok(metadata) => {
            if metadata.len() == 0 {
                Ok(SingleConfig::new())
            } else {
                serde_yaml::from_reader(f)
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
    create_dir_all(s)
}

pub fn ensure_file(filename: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(filename)
}

const PILOTA_CREATED_FILE_NAME: &str = "pilota_crated";

/// Pull the minimal, expected .thrift files from a git repository.
pub fn download_files_from_git(task: Task) -> anyhow::Result<()> {
    ensure_path(&task.dir)?;
    if task.dir.join(PILOTA_CREATED_FILE_NAME).exists() {
        return Ok(());
    }

    git_archive(&task.repo, &task.lock, &task.dir)?;

    Ok(())
}

pub fn download_repo(repo: &Repo, target_dir: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
    let dir = target_dir.as_ref().join(get_git_path(repo.url.as_str())?);
    let task = Task::new(
        vec![],
        dir.clone(),
        repo.url.to_string(),
        repo.lock.to_string(),
    );
    download_files_from_git(task).with_context(|| format!("download repo {}", repo.url))?;
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
    run_command(
        Command::new("git")
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg(repo)
            .current_dir(dir),
    )?;

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

    std::fs::write(dir.join(PILOTA_CREATED_FILE_NAME), "")?;

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
    // open config file and read
    let mut f = open_config_file(DEFAULT_CONFIG_FILE).context("open config file")?;
    let mut config = read_config_from_file(&f).context("read config file")?;

    let r = func(&mut config)?;

    // write back to config file
    f.rewind()?;
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
            if repo.workdir().map_or(false, |workdir| workdir == path) {
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

pub fn download_repos_to_target(
    repos: &HashMap<FastStr, Repo>,
    target_dir: impl AsRef<Path>,
) -> anyhow::Result<HashMap<FastStr, PathBuf>> {
    let mut repo_relative_dir_map = HashMap::with_capacity(repos.len());
    for (name, repo) in repos {
        let dir = download_repo(repo, target_dir.as_ref())?;
        repo_relative_dir_map.insert(name.clone(), dir);
    }
    Ok(repo_relative_dir_map)
}

pub fn get_idl_relative_path(
    idl: &Idl,
    repo_relative_dir_map: &HashMap<FastStr, PathBuf>,
) -> PathBuf {
    if let Source::Git(GitSource { ref repo_name }) = idl.source {
        // git should use relative path instead of absolute path
        let dir = repo_relative_dir_map
            .get(repo_name)
            .expect("git source requires the repo info for idl")
            .clone();
        dir.join(strip_slash_prefix(idl.path.as_path()))
    } else {
        idl.path.clone()
    }
}

pub struct ServiceBuilder {
    pub path: PathBuf,
    pub includes: Vec<PathBuf>,
    pub touch: Vec<String>,
    pub keep_unknown_fields: bool,
}

pub fn get_service_builders_from_services(
    services: &[Service],
    repo_relative_dir_map: &HashMap<FastStr, PathBuf>,
) -> Vec<ServiceBuilder> {
    services
        .iter()
        .map(|s| ServiceBuilder {
            path: get_idl_relative_path(&s.idl, repo_relative_dir_map),
            includes: s.idl.includes.clone(),
            touch: s.codegen_option.touch.clone(),
            keep_unknown_fields: s.codegen_option.keep_unknown_fields,
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
                        "The specified repo '{}' already exists in entry '{}' with different url, \
                         maybe use another repo name, like {}",
                        key,
                        entry_name,
                        get_repo_name_by_url(git)
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
                // check repo by git url rindex
                if url_map.contains_key(&FastStr::new(git)) {
                    bail!(
                        "The specified repo '{}' is indexed by the existed repo name '{}' in \
                         entry '{}', please use the existed repo name",
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
                // check repo by git url rindex
                let repo = url_map.get(&key).unwrap();
                let existed_ref = &repos
                    .get(repo)
                    .expect("the repo index should exist for the git rindex map")
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
                // create a new repo by the git url
                let name = FastStr::new(get_repo_name_by_url(git));
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
            bail!("The specified repo or git should be specified")
        }
    };
    Ok(repo_name)
}

pub fn create_git_service(
    repo_name: FastStr,
    idl_path: &Path,
    includes: &[PathBuf],
) -> Result<Service, anyhow::Error> {
    Ok(Service {
        idl: Idl {
            source: Source::Git(GitSource { repo_name }),
            path: strip_slash_prefix(idl_path),
            includes: includes.to_vec(),
        },
        codegen_option: Default::default(),
    })
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
    fn test_get_idl_relative_path() {
        let idl = Idl {
            source: Source::Local,
            path: PathBuf::from("idl/test.thrift"),
            includes: vec![],
        };
        let repo_relative_dir_map = HashMap::new();
        assert_eq!(
            get_idl_relative_path(&idl, &repo_relative_dir_map),
            idl.path
        );

        let idl = Idl {
            source: Source::Git(GitSource {
                repo_name: "test".into(),
            }),
            path: PathBuf::from("idl/test.thrift"),
            includes: vec![],
        };
        let mut repo_relative_dir_map = HashMap::new();
        repo_relative_dir_map.insert("test".into(), PathBuf::from("repo"));
        assert_eq!(
            get_idl_relative_path(&idl, &repo_relative_dir_map),
            PathBuf::from("repo/idl/test.thrift")
        );
    }

    #[test]
    fn test_get_service_builders_from_services() {
        let idl = Idl {
            source: Source::Local,
            path: PathBuf::from("idl/test.thrift"),
            includes: vec![],
        };
        let service = Service {
            idl: idl.clone(),
            codegen_option: Default::default(),
        };
        let services = vec![service];
        let repo_relative_dir_map = HashMap::new();
        let builders = get_service_builders_from_services(&services, &repo_relative_dir_map);
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
             repo name 'test' in entry 'test_entry', please use the existed repo name"
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
            "The specified repo 'test' already exists in entry 'test_entry' with different url, \
             maybe use another repo name, like repo2"
        );
    }
}
