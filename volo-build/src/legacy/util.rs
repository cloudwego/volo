use std::{
    fs::{create_dir_all, File, OpenOptions},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use serde::de::Error;

use super::model::Config;

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
        .truncate(false)
        .open(filename)
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
