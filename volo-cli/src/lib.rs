#![doc(
    html_logo_url = "https://github.com/cloudwego/volo/raw/main/.github/assets/logo.png?sanitize=true"
)]
#![cfg_attr(not(doctest), doc = include_str!("../README.md"))]

#[macro_use]
mod command;
pub mod context;
mod http;
mod idl;
mod init;
pub mod model;

/// output template file's content to target file, if any params provided,
/// file content will be treated as format string.
#[macro_export]
macro_rules! templates_to_target_file {
    ($folder: ident, $template_file_name: expr, $target_file_name: expr) => {
        let folder = $folder;
        let file_path = folder.join($target_file_name);
        if !file_path.exists() {
            let content = include_bytes!($template_file_name);
            let mut file = std::fs::File::create(file_path)?;
            std::io::Write::write_all(&mut file, content)?;
        }
    };

    ($folder: ident, $template_file_name: expr, $target_file_name: expr, $($args:tt)*) => {
        let folder = $folder;
        let file_path = folder.join($target_file_name);
        if !file_path.exists() {
            let content = format!(include_str!($template_file_name), $($args)*);
            let mut file = std::fs::File::create(file_path)?;
            std::io::Write::write_all(&mut file, content.as_bytes())?;
        }
    };
}
