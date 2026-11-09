use std::path::{Path, PathBuf};

use anyhow::Ok;
use pilota_build::BoxClonePlugin;
use volo::FastStr;

use crate::{
    model::{self, Entry},
    util::{
        DEFAULT_CONFIG_FILE, DEFAULT_DIR, ServiceBuilder, collect_no_service_paths,
        download_repos_to_target, get_service_builders_from_services, open_config_file,
        read_config_from_file,
    },
};

pub struct ConfigBuilder {
    filename: PathBuf,
    plugins: Vec<BoxClonePlugin>,
    out_dir: Option<PathBuf>,
    thrift_grpc_runtime: Option<String>,
}

#[allow(clippy::large_enum_variant)]
pub enum InnerBuilder {
    ThriftGrpc(
        crate::Builder<
            crate::thrift_grpc_backend::MkThriftGrpcBackend,
            pilota_build::parser::ThriftParser,
        >,
    ),
    Protobuf(
        crate::Builder<crate::grpc_backend::MkGrpcBackend, pilota_build::parser::ProtobufParser>,
    ),
    Thrift(
        crate::Builder<crate::thrift_backend::MkThriftBackend, pilota_build::parser::ThriftParser>,
    ),
}

impl InnerBuilder {
    fn for_entry(entry: &Entry, runtime: Option<&str>) -> anyhow::Result<Self> {
        match entry.protocol {
            model::IdlProtocol::Thrift => match model::thrift_grpc_options(&entry.services)? {
                Some(mut options) => {
                    if let Some(runtime) = runtime {
                        options.runtime_crate = Some(runtime.to_owned());
                    }
                    Ok(Self::ThriftGrpc(
                        crate::Builder::thrift_grpc().thrift_grpc_options(options),
                    ))
                }
                None => Ok(Self::thrift()),
            },
            model::IdlProtocol::Protobuf => Ok(Self::protobuf()),
        }
    }

    fn thrift() -> Self {
        InnerBuilder::Thrift(crate::Builder::thrift())
    }

    fn protobuf() -> Self {
        InnerBuilder::Protobuf(crate::Builder::protobuf())
    }

    fn service_configs(
        mut self,
        services: &[model::Service],
        repos: &std::collections::HashMap<FastStr, PathBuf>,
    ) -> Self {
        for service in services {
            let (path, _) = crate::util::get_idl_build_path_and_includes(&service.idl, repos);
            let config = service.codegen_option.config.clone();
            self = match self {
                Self::Protobuf(inner) => Self::Protobuf(inner.service_config(path, config)),
                Self::Thrift(inner) => Self::Thrift(inner.service_config(path, config)),
                Self::ThriftGrpc(inner) => Self::ThriftGrpc(inner.service_config(path, config)),
            };
        }
        self
    }

    fn plugin<P: pilota_build::Plugin + 'static>(self, p: P) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.plugin(p)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.plugin(p)),
            InnerBuilder::ThriftGrpc(inner) => InnerBuilder::ThriftGrpc(inner.plugin(p)),
        }
    }

    fn write(self) -> anyhow::Result<()> {
        match self {
            InnerBuilder::Protobuf(inner) => inner.write(),
            InnerBuilder::Thrift(inner) => inner.write(),
            InnerBuilder::ThriftGrpc(inner) => inner.write(),
        }
    }

    fn init_service(self) -> anyhow::Result<(String, String)> {
        match self {
            InnerBuilder::Protobuf(inner) => inner.init_service(),
            InnerBuilder::Thrift(inner) => inner.init_service(),
            InnerBuilder::ThriftGrpc(inner) => inner.init_service(),
        }
    }

    fn filename(self, filename: PathBuf) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.filename(filename)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.filename(filename)),
            InnerBuilder::ThriftGrpc(inner) => InnerBuilder::ThriftGrpc(inner.filename(filename)),
        }
    }

    fn includes(self, includes: Vec<PathBuf>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.include_dirs(includes)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.include_dirs(includes)),
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.include_dirs(includes))
            }
        }
    }

    fn out_dir<P: AsRef<Path>>(self, out_dir: P) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.out_dir(&out_dir)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.out_dir(&out_dir)),
            InnerBuilder::ThriftGrpc(inner) => InnerBuilder::ThriftGrpc(inner.out_dir(&out_dir)),
        }
    }

    pub fn add_service<P>(self, path: P) -> Self
    where
        P: AsRef<Path>,
    {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.add_service(path)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.add_service(path)),
            InnerBuilder::ThriftGrpc(inner) => InnerBuilder::ThriftGrpc(inner.add_service(path)),
        }
    }

    pub fn add_services(mut self, service_builders: Vec<ServiceBuilder>) -> Self {
        for ServiceBuilder {
            path,
            includes,
            touch,
            keep_unknown_fields,
        } in service_builders
        {
            self = self.add_service(path.clone()).includes(includes);
            if !touch.is_empty() {
                self = self.touch([(path.clone(), touch)]);
            }
            if keep_unknown_fields {
                self = self.keep_unknown_fields([path]);
            }
        }
        self
    }

    pub fn touch(self, items: impl IntoIterator<Item = (PathBuf, Vec<impl Into<String>>)>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.touch(items)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.touch(items)),
            InnerBuilder::ThriftGrpc(inner) => InnerBuilder::ThriftGrpc(inner.touch(items)),
        }
    }

    pub fn ignore_unused(self, ignore_unused: bool) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.ignore_unused(ignore_unused))
            }
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.ignore_unused(ignore_unused)),
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.ignore_unused(ignore_unused))
            }
        }
    }

    pub fn touch_files(self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.touch_files(paths)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.touch_files(paths)),
            InnerBuilder::ThriftGrpc(inner) => InnerBuilder::ThriftGrpc(inner.touch_files(paths)),
        }
    }

    pub fn keep_unknown_fields(self, keep: impl IntoIterator<Item = PathBuf>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.keep_unknown_fields(keep))
            }
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.keep_unknown_fields(keep)),
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.keep_unknown_fields(keep))
            }
        }
    }

    pub fn split_generated_files(self, split_generated_files: bool) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.split_generated_files(split_generated_files))
            }
            InnerBuilder::Thrift(inner) => {
                InnerBuilder::Thrift(inner.split_generated_files(split_generated_files))
            }
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.split_generated_files(split_generated_files))
            }
        }
    }

    pub fn common_crate_name(self, name: FastStr) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.common_crate_name(name)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.common_crate_name(name)),
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.common_crate_name(name))
            }
        }
    }

    pub fn special_namings(self, namings: impl IntoIterator<Item = FastStr>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.special_namings(namings)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.special_namings(namings)),
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.special_namings(namings))
            }
        }
    }

    pub fn dedup(self, dedup_list: Vec<FastStr>) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => InnerBuilder::Protobuf(inner.dedup(dedup_list)),
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.dedup(dedup_list)),
            InnerBuilder::ThriftGrpc(inner) => InnerBuilder::ThriftGrpc(inner.dedup(dedup_list)),
        }
    }

    pub fn with_descriptor(self, with_descriptor: bool) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.with_descriptor(with_descriptor))
            }
            InnerBuilder::Thrift(inner) => {
                InnerBuilder::Thrift(inner.with_descriptor(with_descriptor))
            }
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.with_descriptor(with_descriptor))
            }
        }
    }

    pub fn with_field_mask(self, with_field_mask: bool) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.with_field_mask(with_field_mask))
            }
            InnerBuilder::Thrift(inner) => {
                InnerBuilder::Thrift(inner.with_field_mask(with_field_mask))
            }
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.with_field_mask(with_field_mask))
            }
        }
    }

    pub fn with_comments(self, with_comments: bool) -> Self {
        match self {
            InnerBuilder::Protobuf(inner) => {
                InnerBuilder::Protobuf(inner.with_comments(with_comments))
            }
            InnerBuilder::Thrift(inner) => InnerBuilder::Thrift(inner.with_comments(with_comments)),
            InnerBuilder::ThriftGrpc(inner) => {
                InnerBuilder::ThriftGrpc(inner.with_comments(with_comments))
            }
        }
    }
}

impl ConfigBuilder {
    pub fn new(filename: PathBuf) -> Self {
        ConfigBuilder {
            filename,
            plugins: Vec::new(),
            out_dir: None,
            thrift_grpc_runtime: None,
        }
    }

    /// Select a runtime facade for gRPC Thrift entries. Other entries retain
    /// their existing backend and dependency paths.
    pub fn thrift_grpc_runtime_crate(mut self, runtime: impl Into<String>) -> Self {
        self.thrift_grpc_runtime = Some(runtime.into());
        self
    }

    pub fn plugin<P: pilota_build::ClonePlugin + 'static>(mut self, p: P) -> Self {
        self.plugins.push(BoxClonePlugin::new(p));

        self
    }

    /// Overrides the output directory used by the underlying code generator.
    /// This also relocates downloaded IDL repos from `${OUT_DIR}/idl` to `<out_dir>/idl`.
    pub fn out_dir<P: AsRef<Path>>(mut self, out_dir: P) -> Self {
        self.out_dir = Some(out_dir.as_ref().to_path_buf());
        self
    }

    fn get_out_dir(&self) -> anyhow::Result<PathBuf> {
        if let Some(out_dir) = &self.out_dir {
            return Ok(out_dir.clone());
        }

        // Default to OUT_DIR derived from `DEFAULT_DIR` (which is `${OUT_DIR}/idl`).
        // We intentionally don't check `OUT_DIR` here, because `DEFAULT_DIR` already
        // provides a clear panic message when used outside build.rs.
        Ok(PathBuf::from(DEFAULT_DIR.parent().expect(
            "DEFAULT_DIR should always have a parent directory",
        )))
    }

    pub fn write(self) -> anyhow::Result<()> {
        println!("cargo:rerun-if-changed={}", self.filename.display());
        let mut f = open_config_file(self.filename.clone())?;
        let config = read_config_from_file(&mut f)?;
        let out_dir = self.get_out_dir()?;
        let idl_dir = out_dir.join("idl");
        config
            .entries
            .into_iter()
            .try_for_each(|(entry_name, entry)| {
                let mut builder =
                    InnerBuilder::for_entry(&entry, self.thrift_grpc_runtime.as_deref())?
                        .filename(entry.filename.clone())
                        .out_dir(&out_dir);

                for p in self.plugins.iter() {
                    builder = builder.plugin(p.clone());
                }

                // download repos and get the repo paths
                let target_dir = idl_dir.join(&entry_name);
                let repo_dir_map = download_repos_to_target(&entry.repos, target_dir)?;

                // collect per-IDL no_service flags from `codegen_option.config.no_service`
                let no_service_paths = collect_no_service_paths(&entry.services, &repo_dir_map);

                // get idl builders from services
                let service_builders =
                    get_service_builders_from_services(&entry.services, &repo_dir_map);

                // add build options to the builder and build
                let mut builder = builder
                    .add_services(service_builders)
                    .service_configs(&entry.services, &repo_dir_map);
                if !no_service_paths.is_empty() {
                    builder = builder.touch_files(no_service_paths);
                }
                builder
                    .ignore_unused(!entry.common_option.touch_all)
                    .special_namings(entry.common_option.special_namings)
                    .split_generated_files(entry.common_option.split_generated_files)
                    .dedup(entry.common_option.dedups)
                    .with_descriptor(entry.common_option.with_descriptor)
                    .with_field_mask(entry.common_option.with_field_mask)
                    .with_comments(entry.common_option.with_comments)
                    .write()?;

                Ok(())
            })?;
        Ok(())
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        ConfigBuilder::new(PathBuf::from(DEFAULT_CONFIG_FILE))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use tempfile::tempdir;

    use super::ConfigBuilder;

    fn write_thrift(path: &Path, namespace: &str, type_name: &str) {
        fs::write(
            path,
            format!(
                "namespace rs {namespace}\n\nstruct {type_name} {{\n    1: required string \
                 value,\n}}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn write_respects_service_level_no_service_per_path() {
        let dir = tempdir().unwrap();
        let target_idl = dir.path().join("target.thrift");
        let ignored_idl = dir.path().join("ignored.thrift");
        let config_path = dir.path().join("volo.yml");
        let out_dir = dir.path().join("out");

        write_thrift(&target_idl, "no_service_target", "WantedRecord");
        write_thrift(&ignored_idl, "no_service_ignored", "IgnoredRecord");

        fs::write(
            &config_path,
            format!(
                "entries:\n  sample:\n    filename: generated.rs\n    protocol: thrift\n    services:\n      - idl:\n          source: local\n          path: {}\n        codegen_option:\n          config:\n            no_service: true\n      - idl:\n          source: local\n          path: {}\n",
                target_idl.display(),
                ignored_idl.display()
            ),
        )
        .unwrap();

        ConfigBuilder::new(config_path)
            .out_dir(&out_dir)
            .write()
            .unwrap();

        let generated = fs::read_to_string(out_dir.join("generated.rs")).unwrap();
        assert!(generated.contains("WantedRecord"));
        assert!(!generated.contains("IgnoredRecord"));
    }

    #[test]
    fn get_out_dir_prefers_explicit_out_dir() {
        let explicit = tempfile::tempdir()
            .unwrap()
            .path()
            .join("volo-build-explicit-out");

        let out_dir = ConfigBuilder::new(PathBuf::from("volo.yml"))
            .out_dir(&explicit)
            .get_out_dir()
            .unwrap();

        assert_eq!(out_dir, explicit);
    }
}

pub struct InitBuilder {
    entry: Entry,
}

impl InitBuilder {
    pub fn new(entry: Entry) -> Self {
        InitBuilder { entry }
    }

    pub fn init(self) -> anyhow::Result<(String, String)> {
        let mut builder = InnerBuilder::for_entry(&self.entry, None)?.filename(self.entry.filename);

        // download repos and get the repo paths
        let temp_target_dir = tempfile::TempDir::new()?;
        let repo_dir_map = download_repos_to_target(&self.entry.repos, temp_target_dir.as_ref())?;

        // collect per-IDL no_service flags from `codegen_option.config.no_service`
        let no_service_paths = collect_no_service_paths(&self.entry.services, &repo_dir_map);

        // get idl builders from services
        let idl_builders = get_service_builders_from_services(&self.entry.services, &repo_dir_map);

        // add services to the builder
        builder = builder
            .add_services(idl_builders)
            .service_configs(&self.entry.services, &repo_dir_map);
        if !no_service_paths.is_empty() {
            builder = builder.touch_files(no_service_paths);
        }

        builder.init_service()
    }
}
