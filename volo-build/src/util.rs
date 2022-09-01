use std::{
    fs::{create_dir_all, File, OpenOptions},
    io::{Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{bail, Context};
use lazy_static::lazy_static;

use crate::model::Config;

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
    if f.metadata().unwrap().len() != 0 {
        serde_yaml::from_reader(f)
    } else {
        Ok(Config::new())
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

/// Pull the minimal, expected .thrift files from a git repository.
pub fn download_files_from_git(task: Task) -> anyhow::Result<()> {
    ensure_path(&task.dir)?;

    git_archive(&task.repo, &task.lock, &task.dir)?;

    Ok(())
}

pub fn git_archive(repo: &str, revision: &str, dir: &Path) -> anyhow::Result<()> {
    Command::new("git")
        .arg("init")
        .current_dir(dir)
        .spawn()
        .expect("failed to spawn git archive")
        .wait()?;

    Command::new("git")
        .arg("remote")
        .arg("add")
        .arg("origin")
        .arg(repo)
        .current_dir(dir)
        .spawn()
        .expect("failed to set remote")
        .wait()?;

    Command::new("git")
        .arg("fetch")
        .arg("origin")
        .arg(revision)
        .arg("--depth=1")
        .current_dir(dir)
        .spawn()
        .expect("failed to fetch origin")
        .wait()?;

    Command::new("git")
        .arg("reset")
        .arg("--hard")
        .arg(revision)
        .current_dir(dir)
        .spawn()
        .expect("failed to reset git revision")
        .wait()?;

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
            Command::new("git")
                .arg("ls-remote")
                .arg(repo)
                .arg(r#ref)
                .output()
                .unwrap()
                .stdout,
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
    f.seek(SeekFrom::Start(0))?;
    serde_yaml::to_writer(&mut f, &config).context("write back config file")?;
    let len = f.seek(SeekFrom::Current(0))?;
    f.set_len(len)?;

    Ok(r)
}
