// Shared build-script support. This file is also included directly by the
// Rust fixtures so consumers need neither a published test crate nor a
// prescribed cross-repository checkout layout.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub struct FixturePaths {
    pub interop: PathBuf,
    pub audit: PathBuf,
}

fn replace_once(input: &str, from: &str, to: &str, label: &str) -> io::Result<String> {
    let count = input.matches(from).count();
    if count != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("TTStream fixture schema drift: expected exactly one {label}, found {count}"),
        ));
    }
    Ok(input.replacen(from, to, 1))
}

fn rust_interop_view(source: &str, extension: &str) -> io::Result<String> {
    let source = replace_once(
        source,
        "namespace go interop\n",
        "namespace rs interop\n",
        "Go interop namespace",
    )?;
    // Go default requiredness is intentional and remains the canonical input.
    // Rust's existing view requires both fields. Fail explicitly if Message
    // changes, instead of silently applying the adapter to a different schema.
    let source = replace_once(
        &source,
        "struct Message {\n  1: string value\n  2: i32 seq\n}",
        "struct Message { 1: required string value, 2: required i32 seq }",
        "Message value/seq field block",
    )?;
    if source.contains("service Pure")
        || extension.matches("service Pure {").count() != 1
        || extension.contains("namespace ")
        || extension.contains("struct ")
        || extension.matches("service ").count() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "TTStream fixture schema drift: Pure must be one independent Rust-only service extension",
        ));
    }
    // Keep the previous Rust input text byte-for-byte stable, including runtime
    // generic IDL inputs. This changes indentation only, never method contents.
    let mut result = source
        .lines()
        .map(|line| {
            if line.starts_with("  ") {
                &line[1..]
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    result.push('\n');
    result.push_str(extension);
    Ok(result)
}

/// Prepare only the Rust-specific Interop view in OUT_DIR. Inheritance IDLs
/// already have a shared Rust namespace and are consumed directly from Go's
/// canonical source directory (including its relative parent include).
pub fn prepare(root: &Path, out_dir: &Path) -> io::Result<FixturePaths> {
    let root = root.canonicalize()?;
    let source = root.join("fixtures/interop.thrift");
    let extension = root.join("fixtures/rust-pure-extension.thrift");
    let audit = root.join("go/idl/audit.thrift");
    let parent = root.join("go/idl/audit_parent.thrift");
    for path in [&source, &extension, &audit, &parent] {
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("missing shared TTStream fixture: {}", path.display()),
            ));
        }
    }
    let output = rust_interop_view(
        &fs::read_to_string(source)?,
        &fs::read_to_string(extension)?,
    )?;
    let directory = out_dir.join("shared-fixtures");
    fs::create_dir_all(&directory)?;
    let interop = directory.join("interop.thrift");
    if fs::read_to_string(&interop).ok().as_deref() != Some(&output) {
        fs::write(&interop, output)?;
    }
    Ok(FixturePaths { interop, audit })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inputs() -> (&'static str, &'static str) {
        (
            include_str!("../../fixtures/interop.thrift"),
            include_str!("../../fixtures/rust-pure-extension.thrift"),
        )
    }
    #[test]
    fn rust_view_preserves_service_body_and_go_default_requiredness() {
        let (source, extension) = inputs();
        let view = rust_interop_view(source, extension).unwrap();
        assert!(source.contains("1: string value"));
        assert!(!source.contains("required"));
        assert!(view.contains("1: required string value, 2: required i32 seq"));
        assert_eq!(view.matches("namespace rs interop").count(), 1);
        assert_eq!(view.matches("service Pure").count(), 1);
        let go_methods: Vec<_> = source
            .lines()
            .filter(|line| line.trim_start().starts_with("Message "))
            .map(str::trim)
            .collect();
        let rust_methods: Vec<_> = view
            .lines()
            .filter(|line| line.trim_start().starts_with("Message "))
            .map(str::trim)
            .collect();
        assert_eq!(go_methods, rust_methods);
    }
    #[test]
    fn namespace_or_message_drift_fails_explicitly() {
        let (source, extension) = inputs();
        for input in [
            source.replace("namespace go interop", "namespace go changed"),
            format!("namespace go interop\n{source}"),
            source.replace("2: i32 seq", "2: i64 seq"),
            source.replace("2: i32 seq", "2: i32 seq\n  3: string extra"),
        ] {
            assert_eq!(
                rust_interop_view(&input, extension).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }
    #[test]
    fn duplicate_pure_extension_is_rejected() {
        let (source, extension) = inputs();
        for input in [format!("{source}{extension}"), source.to_string()] {
            let extra = if input == source {
                format!("{extension}{extension}")
            } else {
                extension.to_string()
            };
            assert!(rust_interop_view(&input, &extra).is_err());
        }
    }
}
