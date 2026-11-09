//! Thrift data types with explicit gRPC service adapters.
//!
//! Data, Args/Result wrappers and exception enums come from Pilota. Only the
//! transport adapters are generated here; no Protobuf or TTHeader envelopes are
//! introduced into a Thrift payload.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Context as _, bail};
use itertools::Itertools;
use pilota_build::{
    CodegenBackend, Context, DefId, IdentName, ModPath, ThriftBackend,
    db::RirDatabase,
    rir::{self, Item, Method, MethodSource, NodeKind},
    tags::{PilotaName, RustWrapperArc, thrift::ThriftStreamingMode},
};
use pilota_thrift_reflect::thrift_reflection::FileDescriptor;

/// Options apply to the entire generation entry, never a subset of services.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Full Rust path to the root module of an existing Thrift data generation.
    /// Re-export its types instead of giving the gRPC module new type identities.
    pub shared_types_module: Option<String>,
    /// Override the default thriftgo Go package name for every service.
    pub wire_package: Option<String>,
    /// Original IDL service name -> full wire service name (including package).
    pub wire_services: BTreeMap<String, String>,
    /// Public runtime facade to use in generated adapters; defaults to `::volo_grpc`.
    pub runtime_crate: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct MkThriftGrpcBackend(pub Options);

impl MkThriftGrpcBackend {
    fn effective_options(&self, cx: &Context) -> anyhow::Result<Options> {
        let mut options = crate::model::resolve_thrift_grpc_configs(
            cx.source.services.iter().map(|service| &service.config),
            true,
        )?
        .expect("explicit gRPC builder");
        if self.0.shared_types_module.is_some() {
            options.shared_types_module = self.0.shared_types_module.clone();
        }
        if self.0.wire_package.is_some() {
            options.wire_package = self.0.wire_package.clone();
        }
        if self.0.runtime_crate.is_some() {
            options.runtime_crate = self.0.runtime_crate.clone();
        }
        options.wire_services.extend(self.0.wire_services.clone());
        Ok(options)
    }
}

impl pilota_build::MakeBackend for MkThriftGrpcBackend {
    type Target = ThriftGrpcBackend;

    fn validate(&self, cx: &Context) -> anyhow::Result<()> {
        let options = self.effective_options(cx)?;
        if let Some(module) = &options.shared_types_module {
            syn::parse_str::<syn::Path>(module).context("invalid shared_types_module Rust path")?;
        }
        if let Some(runtime) = &options.runtime_crate {
            syn::parse_str::<syn::Path>(runtime).context("invalid runtime_crate Rust path")?;
        }
        let mut names = BTreeMap::new();
        for (id, node) in cx.nodes().iter() {
            let NodeKind::Item(item) = &node.kind else {
                continue;
            };
            let Item::Service(service) = &**item else {
                continue;
            };
            let name = wire_name(cx, *id, service, &options)?;
            if names.insert(name.clone(), *id).is_some() {
                bail!("duplicate Thrift gRPC wire service: {name}");
            }
            for method in cx.service_methods(*id).iter() {
                if method.oneway {
                    bail!(
                        "Thrift gRPC does not support oneway method {}.{}; use unary void instead",
                        service.name.raw_str(),
                        method.name.raw_str()
                    );
                }
            }
        }
        Ok(())
    }

    fn make_backend(self, cx: Context) -> Self::Target {
        let options = self.effective_options(&cx).expect("validated gRPC options");
        ThriftGrpcBackend {
            inner: ThriftBackend::new(cx),
            options,
        }
    }
}

fn wire_name(
    cx: &Context,
    id: DefId,
    service: &rir::Service,
    options: &Options,
) -> anyhow::Result<String> {
    let raw = service.name.raw_str();
    let name = if let Some(name) = options.wire_services.get(raw.as_str()) {
        name.clone()
    } else {
        let file_id = cx.node(id).unwrap().file_id;
        let file = cx.file(file_id).unwrap();
        let descriptor = FileDescriptor::deserialize(file.descriptor.clone())?;
        let package = match &options.wire_package {
            Some(package) => package.clone(),
            None => descriptor
                .namespaces
                .get("go")
                .and_then(|namespace| namespace.rsplit('.').next())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    cx.file_paths()[&file_id]
                        .file_stem()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                }),
        };
        if package.is_empty() {
            raw.to_string()
        } else {
            format!("{package}.{raw}")
        }
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
    {
        bail!("invalid Thrift gRPC wire service name: {name:?}");
    }
    Ok(name)
}

#[derive(Clone)]
pub struct ThriftGrpcBackend {
    inner: ThriftBackend,
    options: Options,
}

impl ThriftGrpcBackend {
    fn runtime(&self, code: String) -> String {
        match &self.options.runtime_crate {
            Some(runtime) => code.replace("::volo_grpc::", &format!("{runtime}::")),
            None => code,
        }
    }
    fn qualified_type(
        &self,
        service: DefId,
        value: &pilota_build::ty::CodegenTy,
        global: bool,
    ) -> String {
        if global {
            value.global_path("volo_gen").to_string()
        } else {
            value
                .qualified_path(&|did| {
                    let path = self.cx().related_item_path(service, did);
                    if path.starts_with("::")
                        || path.starts_with("super::")
                        || path.starts_with("self::")
                    {
                        path
                    } else {
                        format!("self::{path}").into()
                    }
                })
                .to_string()
        }
    }

    fn ty(&self, service: DefId, ty: &pilota_build::ty::Ty, global: bool) -> String {
        let value = self.cx().codegen_item_ty(ty.kind.clone());
        let value = self.qualified_type(service, &value, global);
        if self
            .cx()
            .tags(ty.tags_id)
            .as_ref()
            .and_then(|t| t.get::<RustWrapperArc>())
            .is_some_and(|t| t.0)
        {
            format!("::std::sync::Arc<{value}>")
        } else {
            value
        }
    }

    fn wrapper(&self, service: DefId, method: &Method, suffix: &str, global: bool) -> String {
        let owner = match method.source {
            MethodSource::Own => service,
            MethodSource::Extend(id) => id,
        };
        let method_name = self
            .cx()
            .node_tags(method.def_id)
            .and_then(|tags| tags.get::<PilotaName>().cloned())
            .map(|name| name.0)
            .unwrap_or_else(|| method.name.raw_str());
        let method_name = if self.cx().cache.names.contains_key(&method.def_id) {
            method_name.to_string()
        } else {
            method_name.upper_camel_ident().to_string()
        };
        let ident = format!("{}{method_name}{suffix}", self.cx().rust_name(owner));
        let path = if global {
            format!(
                "volo_gen::{}",
                self.cx()
                    .item_path(owner)
                    .iter()
                    .map(ToString::to_string)
                    .join("::")
            )
        } else {
            self.cx().cur_related_item_path(owner).to_string()
        };
        match path.rsplit_once("::") {
            Some((prefix, _)) => format!("{prefix}::{ident}"),
            None => ident,
        }
    }

    fn input_ty(&self, id: DefId, method: &Method, server: bool, global: bool) -> String {
        let mode = self.cx().thrift_streaming_mode(method.def_id);
        if !mode.is_streaming() {
            return self.wrapper(id, method, "Request", global);
        }
        let ty = self.ty(id, &method.args[0].ty, global);
        if server
            && matches!(
                mode,
                ThriftStreamingMode::Client | ThriftStreamingMode::Bidirectional
            )
        {
            format!("::volo_grpc::codec::thrift::ThriftRecvStream<{ty}>")
        } else {
            ty
        }
    }

    fn output_ty(&self, id: DefId, method: &Method, server: bool, global: bool) -> String {
        let mode = self.cx().thrift_streaming_mode(method.def_id);
        let ty = if mode.is_streaming() {
            self.ty(id, &method.ret, global)
        } else {
            // Args/Result lowering is authoritative for service-level Arc and
            // container policy. Scalar/void remain inline; containers can wrap
            // individual elements rather than the whole container.
            let owner = match method.source {
                MethodSource::Own => id,
                MethodSource::Extend(id) => id,
            };
            let result = self.wrapper(id, method, "ResultSend", false);
            let ident = result.rsplit("::").next().unwrap();
            let node = self.cx().node(owner).unwrap();
            let result = node
                .related_nodes
                .iter()
                .find_map(|did| {
                    (&*self.cx().rust_name(*did) == ident).then(|| self.cx().expect_item(*did))
                })
                .expect("lowered Thrift unary Result");
            let Item::Enum(result) = &*result else {
                unreachable!()
            };
            let success = result
                .variants
                .iter()
                .find(|variant| variant.id == Some(0))
                .expect("Thrift Result success");
            let value = self.cx().codegen_item_ty(success.fields[0].kind.clone());
            self.qualified_type(id, &value, global)
        };
        if matches!(
            mode,
            ThriftStreamingMode::Server | ThriftStreamingMode::Bidirectional
        ) {
            if server {
                format!("::volo_grpc::thrift::SendStream<{ty}>")
            } else {
                format!("::volo_grpc::codec::thrift::ThriftRecvStream<{ty}>")
            }
        } else if let Some(exception) = &method.exceptions {
            let path = self
                .cx()
                .codegen_item_ty(pilota_build::ty::TyKind::Path(exception.clone()));
            let path = self.qualified_type(id, &path, global);
            format!("::std::result::Result<{ty}, {path}>")
        } else {
            ty
        }
    }

    fn exception_arms(&self, method: &Method, result: &str, to_wire: bool) -> String {
        let Some(exception) = &method.exceptions else {
            return String::new();
        };
        let Item::Enum(enumeration) = &*self.cx().expect_item(exception.did) else {
            unreachable!()
        };
        let path = self.cx().cur_related_item_path(exception.did);
        enumeration
            .variants
            .iter()
            .map(|variant| {
                let variant = self.cx().rust_name(variant.did);
                if to_wire {
                    format!(
                        "::std::result::Result::Err({path}::{variant}(value)) => \
                         {result}::{variant}(value),"
                    )
                } else {
                    format!(
                        "{result}::{variant}(value) => \
                         ::std::result::Result::Err({path}::{variant}(value)),"
                    )
                }
            })
            .join("")
    }

    fn entry(
        &self,
        name: &str,
        paths: &[String],
        variants: &[String],
        types: &[String],
        send: bool,
        exceptions: Option<&[bool]>,
    ) -> String {
        let fields = variants
            .iter()
            .zip(types)
            .map(|(variant, ty)| {
                let field = if send {
                    format!("::volo_grpc::thrift::SendStream<{ty}>")
                } else {
                    format!("::volo_grpc::codec::thrift::ThriftRecvStream<{ty}>")
                };
                format!("{variant}({field}),")
            })
            .join("");
        if send {
            let arms = variants
                .iter()
                .zip(types)
                .enumerate()
                .map(|(index, (variant, ty))| {
                    let classifier = if exceptions.is_some_and(|exceptions| exceptions[index]) {
                        format!(
                            "::std::option::Option::Some(|value| !::std::matches!(value, \
                             {ty}::Ok(_)))"
                        )
                    } else {
                        "::std::option::Option::None".to_owned()
                    };
                    format!(
                        "Self::{variant}(source) => source.into_body(compression, options, \
                         {classifier}),"
                    )
                })
                .join("");
            format!(
                r#"
                pub enum {name} {{ {fields} }}
                impl ::volo_grpc::SendEntryMessage for {name} {{
                    fn payload_kind(&self) -> ::volo_grpc::message::PayloadKind {{ ::volo_grpc::message::PayloadKind::Thrift }}
                    fn into_body(self, compression: ::std::option::Option<::volo_grpc::codec::compression::CompressionEncoding>) -> ::volo_grpc::BoxStream<'static, ::std::result::Result<::volo_grpc::codegen::Frame<::volo_grpc::codegen::Bytes>, ::volo_grpc::Status>> {{
                        self.into_body_with_options(compression, ::volo_grpc::message::CodecOptions::new(::volo_grpc::codec::CodecLimits::default()))
                    }}
                    fn into_body_with_options(self, compression: ::std::option::Option<::volo_grpc::codec::compression::CompressionEncoding>, options: ::volo_grpc::message::CodecOptions) -> ::volo_grpc::BoxStream<'static, ::std::result::Result<::volo_grpc::codegen::Frame<::volo_grpc::codegen::Bytes>, ::volo_grpc::Status>> {{
                        match self {{ {arms} }}
                    }}
                }}
            "#
            )
        } else {
            let arms = paths.iter().zip(variants).zip(types).enumerate().map(|(index, ((path, variant), ty))| {
                let classifier = if exceptions.is_some_and(|exceptions| exceptions[index]) {
                    format!(".with_exception_classifier(|value| !::std::matches!(value, {ty}::Ok(_)))")
                } else { String::new() };
                format!(
                    "::std::option::Option::Some({path:?}) => ::std::result::Result::Ok(Self::{variant}(::volo_grpc::RecvStream::with_decoder(body, kind, compression, ::volo_grpc::codec::thrift::ThriftDecoder::<{ty}>::default().with_context(options.context.clone()){classifier}).with_limits(options.limits).with_context(options.context))),"
                )
            }).join("");
            format!(
                r#"
                pub enum {name} {{ {fields} }}
                impl ::volo_grpc::RecvEntryMessage for {name} {{
                    fn payload_kind() -> ::volo_grpc::message::PayloadKind {{ ::volo_grpc::message::PayloadKind::Thrift }}
                    fn from_body(method: ::std::option::Option<&str>, body: ::volo_grpc::body::BoxBody, kind: ::volo_grpc::codec::decode::Kind, compression: ::std::option::Option<::volo_grpc::codec::compression::CompressionEncoding>) -> ::std::result::Result<Self, ::volo_grpc::Status> {{
                        Self::from_body_with_options(method, body, kind, compression, ::volo_grpc::message::CodecOptions::new(::volo_grpc::codec::CodecLimits::default()))
                    }}
                    fn from_body_with_options(method: ::std::option::Option<&str>, body: ::volo_grpc::body::BoxBody, kind: ::volo_grpc::codec::decode::Kind, compression: ::std::option::Option<::volo_grpc::codec::compression::CompressionEncoding>, options: ::volo_grpc::message::CodecOptions) -> ::std::result::Result<Self, ::volo_grpc::Status> {{
                        match method {{ {arms} _ => ::std::result::Result::Err(::volo_grpc::Status::unimplemented("Method not found")) }}
                    }}
                }}
            "#
            )
        }
    }
}

impl CodegenBackend for ThriftGrpcBackend {
    const PROTOCOL: &'static str = "thrift";
    fn cx(&self) -> &Context {
        self.inner.cx()
    }

    fn codegen_workspace_dependencies(&self) -> &'static str {
        "volo-grpc = { version = \"*\", features = [\"thrift\"] }"
    }

    fn codegen_item_override(&self, id: DefId, stream: &mut String, item: &Item) -> bool {
        if matches!(item, Item::Service(_) | Item::Mod(_)) {
            return false;
        }
        let Some(root) = &self.options.shared_types_module else {
            return false;
        };
        let path = self.cx().item_path(id);
        let tail = path.iter().map(ToString::to_string).join("::");
        stream.push_str(&format!("pub use {root}::{tail};"));
        true
    }

    fn codegen_struct_impl(&self, id: DefId, stream: &mut String, item: &rir::Message) {
        self.inner.codegen_struct_impl(id, stream, item)
    }
    fn codegen_enum_impl(&self, id: DefId, stream: &mut String, item: &rir::Enum) {
        self.inner.codegen_enum_impl(id, stream, item)
    }
    fn codegen_newtype_impl(&self, id: DefId, stream: &mut String, item: &rir::NewType) {
        self.inner.codegen_newtype_impl(id, stream, item)
    }
    fn codegen_file_descriptor(&self, stream: &mut String, file: &rir::File, direct: bool) {
        self.inner.codegen_file_descriptor(stream, file, direct)
    }
    fn codegen_register_mod_file_descriptor(
        &self,
        stream: &mut String,
        mods: &[(ModPath, Arc<PathBuf>)],
    ) {
        self.inner
            .codegen_register_mod_file_descriptor(stream, mods)
    }
    fn codegen_pilota_trait(&self, stream: &mut String) {
        self.inner.codegen_pilota_trait(stream)
    }

    fn codegen_service_method(&self, id: DefId, method: &Method) -> String {
        let name = self.cx().rust_name(method.def_id);
        let input = self.input_ty(id, method, true, false);
        let output = self.output_ty(id, method, true, false);
        self.runtime(format!(
            "fn {name}(&self, request: ::volo_grpc::Request<{input}>) -> impl \
             ::std::future::Future<Output = \
             ::std::result::Result<::volo_grpc::Response<{output}>, ::volo_grpc::Status>> + \
             ::core::marker::Send;"
        ))
    }

    fn codegen_service_method_with_global_path(&self, id: DefId, method: &Method) -> String {
        let name = self.cx().rust_name(method.def_id);
        let input = self.input_ty(id, method, true, true);
        let output = self.output_ty(id, method, true, true);
        self.runtime(format!(
            "async fn {name}(&self, _request: ::volo_grpc::Request<{input}>) -> \
             ::std::result::Result<::volo_grpc::Response<{output}>, ::volo_grpc::Status> {{ \
             ::std::result::Result::Err(::volo_grpc::Status::unimplemented(\"handler not \
             implemented\")) }}"
        ))
    }

    fn codegen_service_impl(&self, id: DefId, stream: &mut String, service: &rir::Service) {
        let start = stream.len();
        let name = self.cx().rust_name(id).to_string();
        let wire =
            wire_name(self.cx(), id, service, &self.options).expect("validated wire service");
        let methods = self.cx().service_methods(id);
        let paths = methods
            .iter()
            .map(|method| format!("/{wire}/{}", method.name.raw_str()))
            .collect_vec();
        let variants = methods
            .iter()
            .map(|method| {
                self.cx()
                    .rust_name(method.def_id)
                    .0
                    .upper_camel_ident()
                    .to_string()
            })
            .collect_vec();
        let mut req_send = Vec::new();
        let mut req_recv = Vec::new();
        let mut res_send = Vec::new();
        let mut res_recv = Vec::new();
        let mut server_arms = String::new();
        let mut client_methods = String::new();
        let mut oneshot_methods = String::new();
        let mut descriptors = String::new();
        for ((method, path), variant) in methods.iter().zip(&paths).zip(&variants) {
            let mode = self.cx().thrift_streaming_mode(method.def_id);
            let unary = !mode.is_streaming();
            let client_streaming = matches!(
                mode,
                ThriftStreamingMode::Client | ThriftStreamingMode::Bidirectional
            );
            let server_streaming = matches!(
                mode,
                ThriftStreamingMode::Server | ThriftStreamingMode::Bidirectional
            );
            let method_name = self.cx().rust_name(method.def_id);
            let (qs, qr, rs, rr) = if unary {
                let args = self.wrapper(id, method, "ArgsSend", false);
                let alias = self.wrapper(id, method, "Request", false);
                // The alias belongs to the declaring service, just like Args/Result.
                if matches!(method.source, MethodSource::Own) {
                    stream.push_str(&format!("pub type {alias} = {args};"));
                }
                (
                    args,
                    self.wrapper(id, method, "ArgsRecv", false),
                    self.wrapper(id, method, "ResultSend", false),
                    self.wrapper(id, method, "ResultRecv", false),
                )
            } else {
                let req = self.ty(id, &method.args[0].ty, false);
                let resp = self.ty(id, &method.ret, false);
                (req.clone(), req, resp.clone(), resp)
            };
            req_send.push(qs.clone());
            req_recv.push(qr);
            res_send.push(rs.clone());
            res_recv.push(rr.clone());
            let rpc_mode = if unary {
                "Unary"
            } else {
                match mode {
                    ThriftStreamingMode::Client => "ClientStreaming",
                    ThriftStreamingMode::Server => "ServerStreaming",
                    _ => "Bidirectional",
                }
            };
            let raw_method = method.name.raw_str();
            let shape = if unary {
                "UnaryArgsResult"
            } else {
                "StreamingRawStruct"
            };
            descriptors.push_str(&format!(
                "::volo_grpc::message::MethodDescriptor {{ name: {:?}, mode: \
                 ::volo_grpc::message::RpcMode::{rpc_mode}, payload: \
                 ::volo_grpc::message::PayloadShape::{shape} }},",
                method.name.raw_str().as_str()
            ));

            let extract = if client_streaming {
                "let request = ::volo_grpc::Request::from_parts(metadata, extensions, messages);"
                    .to_owned()
            } else if unary {
                let fields = method
                    .args
                    .iter()
                    .map(|arg| {
                        let field = self.cx().rust_name(arg.def_id);
                        format!("{field}: value.{field},")
                    })
                    .join("");
                format!(
                    "let value = ::volo_grpc::thrift::read_unary_request(&mut messages).await?; \
                     if let ::std::option::Option::Some(trailers) = messages.trailers().await? {{ \
                     metadata.merge(trailers); }} let request = \
                     ::volo_grpc::Request::from_parts(metadata, extensions, {qs} {{ {fields} }});"
                )
            } else {
                "let value = messages.message().await?.ok_or_else(|| \
                 ::volo_grpc::Status::invalid_argument(\"Missing server-streaming request \
                 message\"))?; let request = ::volo_grpc::Request::from_parts(metadata, \
                 extensions, value);"
                    .to_owned()
            };
            let response = if server_streaming {
                "response".to_owned()
            } else {
                let wrap = if unary {
                    if method.exceptions.is_some() {
                        let arms = self.exception_arms(method, &rs, true);
                        format!(
                            "match value {{ ::std::result::Result::Ok(value) => {rs}::Ok(value), \
                             {arms} }}"
                        )
                    } else {
                        format!("{rs}::Ok(value)")
                    }
                } else {
                    "value".to_owned()
                };
                format!(
                    "response.map(|value| ::volo_grpc::thrift::SendStream::from_stream(::volo_grpc::codegen::futures::stream::once(::volo_grpc::codegen::futures::future::ready(::std::result::Result::Ok({wrap})))))"
                )
            };
            server_arms.push_str(&format!(
                r#"
                {name}RequestRecv::{variant}(mut messages) => {{
                    {extract}
                    let response = self.inner.{method_name}(request).await?;
                    ::std::result::Result::Ok({response}.map({name}ResponseSend::{variant}))
                }}
            "#
            ));

            let input = if client_streaming {
                format!("impl ::volo_grpc::thrift::IntoStreamingRequest<Message = {qs}>")
            } else {
                format!(
                    "impl ::volo_grpc::IntoRequest<{}>",
                    self.input_ty(id, method, false, false)
                )
            };
            let output = self.output_ty(id, method, false, false);
            let request = if client_streaming {
                "::volo_grpc::thrift::IntoStreamingRequest::into_streaming_request(request)"
            } else {
                "request.into_request().map(|value| ::volo_grpc::thrift::SendStream::from_stream(::volo_grpc::codegen::futures::stream::once(::volo_grpc::codegen::futures::future::ready(::std::result::Result::Ok(value)))))"
            };
            let finish = if server_streaming {
                "::std::result::Result::Ok(::volo_grpc::Response::from_parts(metadata, extensions, \
                 messages))"
                    .to_owned()
            } else {
                let unwrap = if unary {
                    if method.exceptions.is_some() {
                        let arms = self.exception_arms(method, &rr, false);
                        format!(
                            "match value {{ {rr}::Ok(value) => ::std::result::Result::Ok(value), \
                             {arms} }}"
                        )
                    } else {
                        format!("match value {{ {rr}::Ok(value) => value }}")
                    }
                } else {
                    "value".to_owned()
                };
                format!(
                    "let value = ::volo_grpc::thrift::read_unary_response(&mut messages).await?; \
                     if let ::std::option::Option::Some(trailers) = messages.trailers().await? {{ \
                     metadata.merge(trailers); }} \
                     ::std::result::Result::Ok(::volo_grpc::Response::from_parts(metadata, \
                     extensions, {unwrap}))"
                )
            };
            for oneshot in [false, true] {
                let receiver = if oneshot { "self" } else { "&self" };
                let call = if oneshot {
                    "::volo::client::OneShotService::call(self.0, &mut cx, request)"
                } else {
                    "::volo::Service::call(&self.0, &mut cx, request)"
                };
                let code = format!(
                    r#"
                    pub async fn {method_name}({receiver}, request: {input}) -> ::std::result::Result<::volo_grpc::Response<{output}>, ::volo_grpc::Status> {{
                        let mut request = {request}.map(|messages| {name}RequestSend::{variant}(messages));
                        request.extensions_mut().insert(::volo_grpc::message::MethodDescriptor {{ name: {raw_method:?}, mode: ::volo_grpc::message::RpcMode::{rpc_mode}, payload: ::volo_grpc::message::PayloadShape::{shape} }});
                        let mut cx = self.0.make_cx({path:?});
                        let response = {call}.await?;
                        let (mut metadata, extensions, entry) = response.into_parts();
                        let mut messages = match entry {{ {name}ResponseRecv::{variant}(messages) => messages, #[allow(unreachable_patterns)] _ => return ::std::result::Result::Err(::volo_grpc::Status::internal("Mismatched response method")) }};
                        {finish}
                    }}
                "#
                );
                if oneshot {
                    oneshot_methods.push_str(&code);
                } else {
                    client_methods.push_str(&code);
                }
            }
        }
        let exceptions = methods
            .iter()
            .map(|method| method.exceptions.is_some())
            .collect_vec();
        for (entry, types, send, exceptions) in [
            (format!("{name}RequestSend"), req_send, true, None),
            (format!("{name}RequestRecv"), req_recv, false, None),
            (
                format!("{name}ResponseSend"),
                res_send,
                true,
                Some(exceptions.as_slice()),
            ),
            (
                format!("{name}ResponseRecv"),
                res_recv,
                false,
                Some(exceptions.as_slice()),
            ),
        ] {
            stream.push_str(&self.entry(&entry, &paths, &variants, &types, send, exceptions));
        }
        stream.push_str(&crate::grpc_backend::client_code(
            &name,
            &format!("{name}RequestSend"),
            &format!("{name}ResponseRecv"),
            &client_methods,
            &oneshot_methods,
        ));
        stream.push_str(&format!(r#"
            pub struct {name}Server<S> {{ inner: ::std::sync::Arc<S> }}
            impl<S> ::core::clone::Clone for {name}Server<S> {{ fn clone(&self) -> Self {{ Self {{ inner: self.inner.clone() }} }} }}
            impl<S> {name}Server<S> {{
                pub fn new(inner: S) -> Self {{ Self::from_arc(::std::sync::Arc::new(inner)) }}
                pub fn from_arc(inner: ::std::sync::Arc<S>) -> Self {{ Self {{ inner }} }}
            }}
            impl<S: {name} + ::core::marker::Send + ::core::marker::Sync + 'static> ::volo::Service<::volo_grpc::context::ServerContext, ::volo_grpc::Request<{name}RequestRecv>> for {name}Server<S> {{
                type Response = ::volo_grpc::Response<{name}ResponseSend>;
                type Error = ::volo_grpc::Status;
                async fn call(&self, _cx: &mut ::volo_grpc::context::ServerContext, request: ::volo_grpc::Request<{name}RequestRecv>) -> ::std::result::Result<Self::Response, Self::Error> {{
                    let (mut metadata, extensions, entry) = request.into_parts();
                    match entry {{ {server_arms} }}
                }}
            }}
            impl<S: {name}> ::volo_grpc::server::NamedService for {name}Server<S> {{
                const NAME: &'static str = {wire:?};
                fn descriptor() -> ::std::option::Option<::volo_grpc::message::ServiceDescriptor> {{
                    ::std::option::Option::Some(::volo_grpc::message::ServiceDescriptor {{ payload: ::volo_grpc::message::PayloadKind::Thrift, methods: &[{descriptors}] }})
                }}
            }}
        "#));
        if self.options.runtime_crate.is_some() {
            let code = self.runtime(stream[start..].to_owned());
            stream.truncate(start);
            stream.push_str(&code);
        }
    }
}
