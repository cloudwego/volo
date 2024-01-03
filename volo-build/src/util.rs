use std::{
    fs::{create_dir_all, File, OpenOptions},
    io::Seek,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, Context};
use itertools::Itertools;
use lazy_static::lazy_static;
use serde::de::Error;

use crate::model::{Config, GitSource, Idl, Source};

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

pub fn read_config_from_file(f: &File) -> Result<Config, serde_yaml::Error> {
    match f.metadata() {
        Ok(metadata) => {
            if metadata.len() == 0 {
                Ok(Config::new())
            } else {
                serde_yaml::from_reader(f)
            }
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(Config::new())
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

pub struct LocalIdl {
    pub path: PathBuf,
    pub includes: Vec<PathBuf>,
    pub touch: Vec<String>,
    pub keep_unknown_fields: bool,
}

pub fn get_or_download_idl(idl: Idl, target_dir: impl AsRef<Path>) -> anyhow::Result<LocalIdl> {
    let (path, includes) = if let Source::Git(GitSource {
        ref repo, ref lock, ..
    }) = idl.source
    {
        let lock = lock.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "please exec 'volo idl update' or specify the lock for {}",
                repo
            )
        })?;
        let dir = target_dir
            .as_ref()
            .join(get_git_path(repo.as_str())?)
            .join(lock);
        let task = Task::new(
            vec![idl.path.to_string_lossy().to_string()],
            dir.clone(),
            repo.clone(),
            lock.to_string(),
        );
        download_files_from_git(task).with_context(|| format!("download repo {repo}"))?;

        (
            // git should use relative path instead of absolute path
            dir.join(strip_slash_prefix(idl.path.as_path())),
            idl.includes
                .unwrap_or_default()
                .into_iter()
                .map(|v| dir.join(v))
                .collect_vec(),
        )
    } else {
        (idl.path.to_path_buf(), idl.includes.unwrap_or_default())
    };

    Ok(LocalIdl {
        path,
        includes,
        touch: idl.touch,
        keep_unknown_fields: idl.keep_unknown_fields,
    })
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

pub fn get_repo_latest_commit_id(repo: &str, r#ref: &str) -> anyhow::Result<String> {
    let commit_list = unsafe {
        String::from_utf8_unchecked(
            match Command::new("git")
                .arg("ls-remote")
                .arg(repo)
                .arg(r#ref)
                .output()
            {
                Ok(output) => output.stdout,
                Err(e) => {
                    bail!("git ls-remote {} {} err:{}", repo, r#ref, e);
                }
            },
        )
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
                "get latest commit of {}:{} failed because of multiple refs, please choose one \
                 of: \n{}",
                repo,
                r#ref,
                possibilities,
            );
        }
    }
    let commit_id = commit_list[0][0];
    Ok(commit_id.into())
}

pub fn with_config<F, R>(func: F) -> anyhow::Result<R>
where
    F: FnOnce(&mut Config) -> anyhow::Result<R>,
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
}
