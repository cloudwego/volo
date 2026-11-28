//! Generation helpers shared with the independent StreamX build plugin.

use itertools::Itertools;
use pilota_build::{
    Context, DefId, IdentName,
    db::RirDatabase,
    middle::context::SourceType,
    rir::{self, Method},
    tags::{
        RustWrapperArc,
        protobuf::{ClientStreaming, ServerStreaming},
        thrift::ThriftStreamingMode,
    },
};

use crate::thrift_grpc_backend::{Options as ThriftGrpcOptions, ThriftGrpcBackend};

/// Names and variants shared with plugins that attach traits to unary envelopes.
/// The original Args/Result types remain owned by the selected backend.
#[derive(Clone, Debug)]
pub struct UnaryEnvelopes {
    pub request: String,
    pub response: String,
    pub native_response: Option<String>,
    pub methods: Vec<UnaryMethod>,
}

#[derive(Clone, Debug)]
pub struct UnaryMethod {
    pub name: String,
    pub variant: String,
    pub oneway: bool,
    /// Original ResultSend path, relative to the service's enclosing module.
    pub result: String,
}

pub fn module_name(cx: &Context, service: DefId) -> String {
    format!("{}_streamx", cx.rust_name(service).0.snake_ident())
}

/// Describes the generated ordinary Thrift envelopes without inspecting emitted text.
pub fn unary_envelopes(cx: &Context, service: DefId) -> Option<UnaryEnvelopes> {
    if !matches!(cx.source.source_type, SourceType::Thrift) {
        return None;
    }
    let module = module_name(cx, service);
    let backend = ThriftGrpcBackend::for_streamx(cx.clone(), service).ok()?;
    let grpc_backend = cx.with_adjust(service, |adjust| {
        adjust
            .and_then(|adjust| adjust.codegen_data::<ThriftGrpcOptions>())
            .is_some()
    });
    Some(UnaryEnvelopes {
        request: format!("{module}::ThriftRequest"),
        response: format!("{module}::ThriftResponse"),
        native_response: (!grpc_backend).then(|| format!("{}ResponseSend", cx.rust_name(service))),
        methods: cx
            .service_methods(service)
            .iter()
            .filter(|method| matches!(mode(cx, method, true), Mode::Unary))
            .map(|method| {
                let result = backend.wrapper(service, method, "ResultSend", false);
                UnaryMethod {
                    name: method.name.raw_str().to_string(),
                    variant: cx
                        .rust_name(method.def_id)
                        .0
                        .upper_camel_ident()
                        .to_string(),
                    oneway: method.oneway,
                    result: result.strip_prefix("super::").unwrap_or(&result).to_owned(),
                }
            })
            .collect(),
    })
}

#[derive(Clone, Debug)]
pub struct Options {
    pub runtime_crate: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            runtime_crate: "::volo_streamx".into(),
        }
    }
}

pub(crate) fn nested_path(path: &str) -> String {
    if path.starts_with("::") || path.starts_with("crate::") {
        path.to_owned()
    } else {
        format!("super::{}", path.strip_prefix("self::").unwrap_or(path))
    }
}

fn ty(cx: &Context, service: DefId, value: &pilota_build::ty::Ty) -> String {
    let result = cx
        .codegen_item_ty(value.kind.clone())
        .qualified_path(&|id| nested_path(&cx.related_item_path(service, id)).into())
        .to_string();
    if cx
        .tags(value.tags_id)
        .as_ref()
        .and_then(|tags| tags.get::<RustWrapperArc>())
        .is_some_and(|tag| tag.0)
    {
        format!("::std::sync::Arc<{result}>")
    } else {
        result
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Unary,
    Client,
    Server,
    Bidi,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Unary => "Unary",
            Self::Client => "ClientStreaming",
            Self::Server => "ServerStreaming",
            Self::Bidi => "Bidirectional",
        }
    }
    fn client(self) -> bool {
        matches!(self, Self::Client | Self::Bidi)
    }
    fn server(self) -> bool {
        matches!(self, Self::Server | Self::Bidi)
    }
}

fn mode(cx: &Context, method: &Method, thrift: bool) -> Mode {
    if thrift {
        match cx.thrift_streaming_mode(method.def_id) {
            ThriftStreamingMode::Client => Mode::Client,
            ThriftStreamingMode::Server => Mode::Server,
            ThriftStreamingMode::Bidirectional => Mode::Bidi,
            _ => Mode::Unary,
        }
    } else {
        match (
            cx.node_contains_tag::<ClientStreaming>(method.def_id),
            cx.node_contains_tag::<ServerStreaming>(method.def_id),
        ) {
            (false, false) => Mode::Unary,
            (true, false) => Mode::Client,
            (false, true) => Mode::Server,
            (true, true) => Mode::Bidi,
        }
    }
}

fn unary_input(id: DefId, method: &Method, backend: &ThriftGrpcBackend) -> String {
    match method.args.as_slice() {
        [] => "()".into(),
        [arg] => {
            let value = backend.unary_argument_ty(id, method, 0, false);
            if arg.kind == rir::FieldKind::Optional {
                format!("::std::option::Option<{value}>")
            } else {
                value
            }
        }
        _ => backend.wrapper(id, method, "ArgsRecv", false),
    }
}

fn unary_request(cx: &Context, method: &Method) -> String {
    match method.args.as_slice() {
        [] => "()".into(),
        [arg] => format!("value.{}", cx.rust_name(arg.def_id)),
        _ => "value".into(),
    }
}

/// Generate an attached module beside the original service and message types.
pub fn generate(
    cx: &Context,
    id: DefId,
    service: &rir::Service,
    options: &Options,
) -> anyhow::Result<(String, String)> {
    syn::parse_str::<syn::Path>(&options.runtime_crate)?;
    crate::util::validate_method_variants(cx, id, crate::util::camel_case_variant)?;
    let thrift = matches!(cx.source.source_type, SourceType::Thrift);
    let backend = if thrift {
        Some(ThriftGrpcBackend::for_streamx(cx.clone(), id)?)
    } else {
        None
    };
    let name = module_name(cx, id);
    let service_name = service.name.raw_str().to_string();
    let wire_name = if let Some(backend) = &backend {
        backend.wire_name(id, service)?
    } else {
        let file = cx.file(cx.node(id).unwrap().file_id).unwrap();
        let package = file.package.iter().join(".");
        if package.is_empty() {
            service_name.clone()
        } else {
            format!("{package}.{service_name}")
        }
    };
    let methods = cx.service_methods(id);
    let mut signatures = String::new();
    let mut descriptors = String::new();
    let mut native_descriptors = String::new();
    let mut variants = Vec::new();
    let mut paths = Vec::new();
    let mut requests = Vec::new();
    let mut replies = Vec::new();
    let mut exceptions = Vec::new();
    let mut grpc_arms = String::new();
    let mut unary_arms = String::new();
    let mut registrations = String::new();
    let mut unary_names = Vec::new();
    let mut unary_variants = Vec::new();
    let mut unary_inputs = Vec::new();
    let mut unary_outputs = Vec::new();
    for method in methods.iter() {
        let method_name = cx.rust_name(method.def_id);
        let variant = method_name.0.upper_camel_ident().to_string();
        let rpc_mode = mode(cx, method, thrift);
        let mode_name = rpc_mode.name();
        let raw = method.name.raw_str().to_string();
        let input = if thrift && matches!(rpc_mode, Mode::Unary) {
            unary_input(id, method, backend.as_ref().unwrap())
        } else if thrift {
            backend
                .as_ref()
                .unwrap()
                .argument_ty(id, &method.args[0], false)
        } else {
            ty(cx, id, &method.args[0].ty)
        };
        let output = if let Some(backend) = &backend {
            if matches!(rpc_mode, Mode::Unary) {
                backend.output_ty(id, method, true, false)
            } else {
                backend.ty(id, &method.ret, false)
            }
        } else {
            ty(cx, id, &method.ret)
        };
        let args = if rpc_mode.client() {
            format!("input: __runtime::RequestStream<{input}>")
        } else {
            format!("req: {input}")
        };
        let (args, result) = if rpc_mode.server() {
            (
                format!("{args}, output: __runtime::ResponseSender<{output}>"),
                "()".to_owned(),
            )
        } else {
            (args, output.clone())
        };
        signatures.push_str(&format!(
            "fn {method_name}(&self, cx: __runtime::StreamContext, {args}) -> impl \
             ::std::future::Future<Output = __runtime::RpcResult<{result}>> + \
             ::core::marker::Send;"
        ));
        descriptors.push_str(&format!(
            "__runtime::MethodDescriptor {{ name:{raw:?}, mode:__runtime::RpcMode::{mode_name}, \
             oneway:{} }},",
            method.oneway
        ));

        if thrift && matches!(rpc_mode, Mode::Unary) {
            let backend = backend.as_ref().unwrap();
            let request = unary_request(cx, method);
            let result = backend.wrapper(id, method, "ResultSend", false);
            let wrap = wrap_result(backend, method, &result);
            unary_arms.push_str(&format!(
                "ThriftRequest::{variant}(value) => {{ let request={request}; let \
                 value=__thrift::streaming::dispatch_unary(cx,{service_name:?},move |cx| \
                 self.inner.{method_name}(cx,request)).await?; \
                 ::std::result::Result::Ok(ThriftResponse::{variant}({wrap})) }},"
            ));
            unary_names.push(raw.clone());
            unary_variants.push(variant.clone());
            unary_inputs.push(backend.wrapper(id, method, "ArgsRecv", false));
            unary_outputs.push(result);
            registrations.push_str(&format!(
                "router.add_unary_method({service_name:?},{raw:?}).expect(\"generated unary \
                 declaration\");"
            ));
        } else if thrift {
            let stream_mode = match rpc_mode {
                Mode::Client => "Client",
                Mode::Server => "Server",
                _ => "Bidirectional",
            };
            let body = if matches!(rpc_mode, Mode::Client) {
                format!(
                    "let (input,mut output)=stream.into_shared::<{input},{output}>(); let \
                     value=inner.{method_name}(cx.clone(),input).await?; output.send(&value).await"
                )
            } else if matches!(rpc_mode, Mode::Server) {
                format!(
                    "let (mut input,output)=stream.into_shared::<{input},{output}>(); let \
                     request=input.recv().await?.ok_or_else(|| __runtime::RpcError::Application \
                     {{ kind:6,message:\"missing request\".into() }})?; drop(input); \
                     inner.{method_name}(cx.clone(),request,output).await"
                )
            } else {
                format!(
                    "let (input,output)=stream.into_shared::<{input},{output}>(); \
                     inner.{method_name}(cx.clone(),input,output).await"
                )
            };
            registrations.push_str(&format!(
                "{{ let inner=self.inner.clone(); \
                 router.add_mode({service_name:?},{raw:?},\
                 __thrift::streaming::StreamMode::{stream_mode},move |native,stream| {{let \
                 inner=inner.clone();async move {{let cx=native.shared_context();cx.scope(async \
                 {{{body}}}).await.map_err(__thrift::streaming::StreamError::from)}}}}).expect(\"\
                 generated streaming declaration\"); }}"
            ));
        }
        if method.oneway {
            continue;
        }
        let shape = if thrift {
            if matches!(rpc_mode, Mode::Unary) {
                "UnaryArgsResult"
            } else {
                "StreamingRawStruct"
            }
        } else {
            "ProtobufMessage"
        };
        native_descriptors.push_str(&format!(
            "__grpc::message::MethodDescriptor \
             {{name:{raw:?},mode:__grpc::message::RpcMode::{mode_name},payload:\
             __grpc::message::PayloadShape::{shape}}},"
        ));
        variants.push(variant.clone());
        paths.push(format!("/{wire_name}/{raw}"));
        let req_wire = if thrift && matches!(rpc_mode, Mode::Unary) {
            backend
                .as_ref()
                .unwrap()
                .wrapper(id, method, "ArgsRecv", false)
        } else {
            input.clone()
        };
        let resp_wire = if thrift && matches!(rpc_mode, Mode::Unary) {
            backend
                .as_ref()
                .unwrap()
                .wrapper(id, method, "ResultSend", false)
        } else {
            output.clone()
        };
        requests.push(req_wire);
        replies.push(resp_wire.clone());
        exceptions.push(method.exceptions.is_some());
        let prelude = if rpc_mode.client() {
            String::new()
        } else {
            let require_end = matches!(rpc_mode, Mode::Unary);
            let request = if thrift && matches!(rpc_mode, Mode::Unary) {
                unary_request(cx, method)
            } else {
                "value".into()
            };
            format!(
                "let value=__grpc::streaming::response::request_one(&input_context,input,\
                 {require_end}).await?; let request={request};"
            )
        };
        let args = if rpc_mode.client() {
            "input"
        } else {
            "request"
        };
        let body = if rpc_mode.server() {
            format!("{prelude} inner.{method_name}(cx,{args},output).await")
        } else {
            let wrap = if thrift && matches!(rpc_mode, Mode::Unary) {
                wrap_result(backend.as_ref().unwrap(), method, &resp_wire)
            } else {
                "value".into()
            };
            format!(
                "{prelude} let value=inner.{method_name}(cx,{args}).await?; \
                 ::std::result::Result::Ok({wrap})"
            )
        };
        let payload = if thrift { "thrift" } else { "protobuf" };
        let call = if rpc_mode.server() {
            format!(
                "__grpc::streaming::response::{payload}(native,config,move|cx,output|async move \
                 {{{body}}}).await?"
            )
        } else {
            format!(
                "__grpc::streaming::response::{payload}_unary(native,move|cx|async move \
                 {{{body}}}).await?"
            )
        };
        let config = if rpc_mode.server() {
            "let config=__grpc::streaming::response::ChannelConfig::for_options(&options);"
        } else {
            ""
        };
        let input = if rpc_mode.client() {
            "let input=__grpc::streaming::response::request(&native,messages);"
        } else {
            "let input=messages; let input_context=native.clone();"
        };
        grpc_arms.push_str(&format!(
            "H2Request::{variant}(messages) => {{ {config} {input} let inner=self.inner.clone(); \
             let response={call}; ::std::result::Result::Ok(response.map(H2Response::{variant})) \
             }},"
        ));
    }
    let native_payload = if thrift { "Thrift" } else { "Protobuf" };
    let (grpc_field, grpc_init, grpc_clone) = if variants.is_empty() {
        ("", "", "")
    } else {
        (
            "grpc_route: ::std::option::Option<__grpc::server::ServiceRoute>,",
            "grpc_route: ::std::option::Option::None,",
            "grpc_route: self.grpc_route.clone(),",
        )
    };
    let runtime = &options.runtime_crate;
    let mut code = format!(
        r#"
        pub mod {name} {{
            use __runtime::codegen::{{grpc as __grpc, thrift as __thrift, volo as __volo}};
            use {runtime} as __runtime;
            pub trait Handler: ::core::marker::Send + ::core::marker::Sync + 'static {{ {signatures} }}
            pub static DESCRIPTOR: __runtime::ServiceDescriptor = __runtime::ServiceDescriptor {{ name:{service_name:?}, wire_name:{wire_name:?}, payload:__runtime::PayloadKind::{native_payload}, methods:&[{descriptors}] }};
            pub struct BoundService<H> {{ inner: ::std::sync::Arc<H>, {grpc_field} }}
            impl<H> ::core::clone::Clone for BoundService<H> {{ fn clone(&self)->Self{{Self{{inner:self.inner.clone(), {grpc_clone}}}}} }}
            pub fn bind<H:Handler>(handler:H)->BoundService<H>{{BoundService{{inner: ::std::sync::Arc::new(handler), {grpc_init}}}}}
            impl<H:Handler> BoundService<H> {{
                pub fn from_arc(inner: ::std::sync::Arc<H>)->Self{{Self{{inner, {grpc_init}}}}}
                pub fn handler(&self)->&::std::sync::Arc<H>{{&self.inner}}
                pub fn descriptor(&self)->&'static __runtime::ServiceDescriptor{{&DESCRIPTOR}}
            }}
    "#
    );
    if !variants.is_empty() {
        if let Some(backend) = &backend {
            code.push_str(&backend.entry("H2Request", &paths, &variants, &requests, false, None));
            code.push_str(&backend.entry(
                "H2Response",
                &paths,
                &variants,
                &replies,
                true,
                Some(&exceptions),
            ));
        } else {
            code.push_str(&protobuf_entries(&paths, &variants, &requests, &replies));
        }
        code.push_str(&format!(r#"
            pub struct GrpcService<H>{{inner: ::std::sync::Arc<H>}}
            impl<H> ::core::clone::Clone for GrpcService<H>{{fn clone(&self)->Self{{Self{{inner:self.inner.clone()}}}}}}
            impl<H:Handler> BoundService<H>{{
                /// Returns the native typed service. Configure its codec when adding it to an explicit gRPC server.
                pub fn grpc(&self)->GrpcService<H>{{GrpcService{{inner:self.inner.clone()}}}}
                /// Configures the native gRPC codec, layers and hooks used by StreamX registration.
                pub fn configure_grpc<L>(mut self, configure: impl ::core::ops::FnOnce(
                    __grpc::server::ServiceBuilder<GrpcService<H>, __volo::layer::Identity>
                )->__grpc::server::ServiceBuilder<GrpcService<H>, L>)->Self
                where
                    L: __volo::Layer<GrpcService<H>>,
                    L::Service: __volo::Service<__grpc::context::ServerContext, __grpc::Request<H2Request>, Response=__grpc::Response<H2Response>>
                        + __grpc::server::NamedService + ::core::clone::Clone + ::core::marker::Send + ::core::marker::Sync + 'static,
                    <L::Service as __volo::Service<__grpc::context::ServerContext, __grpc::Request<H2Request>>>::Error: ::core::convert::Into<__grpc::Status>,
                {{
                    let codec=configure(__grpc::server::ServiceBuilder::new(self.grpc())).build::<H2Request,H2Response>();
                    self.grpc_route=::std::option::Option::Some(__grpc::server::ServiceRoute::new(codec));
                    self
                }}
            }}
            impl<H:Handler> __grpc::server::NamedService for GrpcService<H>{{
                fn requires_managed_transport()->bool{{true}}
                const NAME:&'static str={wire_name:?};
                fn descriptor()->::std::option::Option<__grpc::message::ServiceDescriptor>{{::std::option::Option::Some(__grpc::message::ServiceDescriptor{{payload:__grpc::message::PayloadKind::{native_payload},methods:&[{native_descriptors}]}})}}
            }}
            impl<H:Handler> __volo::Service<__grpc::context::ServerContext,__grpc::Request<H2Request>> for GrpcService<H>{{
                type Response=__grpc::Response<H2Response>;type Error=__grpc::Status;
                async fn call(&self,_:&mut __grpc::context::ServerContext,request:__grpc::Request<H2Request>)->::std::result::Result<Self::Response,Self::Error>{{
                    let (_,extensions,entry)=request.into_parts();
                    let native=extensions.get::<__grpc::streaming::StreamContext>().cloned().ok_or_else(||__grpc::Status::internal("missing managed RPC context"))?;
                    let options=extensions.get::<__grpc::message::CodecOptions>().cloned().unwrap_or_else(||__grpc::message::CodecOptions::new(__grpc::message::PayloadKind::{native_payload}.default_limits()));
                    let _=&options;
                    match entry{{{grpc_arms}}}
                }}
            }}
        "#));
    }
    if thrift {
        code.push_str(&crate::thrift_backend::entry_message(
            "ThriftRequest",
            &unary_names,
            &unary_variants,
            &unary_inputs,
            false,
        ));
        code.push_str(&crate::thrift_backend::entry_message(
            "ThriftResponse",
            &unary_names,
            &unary_variants,
            &unary_outputs,
            true,
        ));
        let method_names = methods
            .iter()
            .map(|m| format!("{:?}", m.name.raw_str().to_string()))
            .join(",");
        code.push_str(&format!(r#"
            pub struct ThriftService<H>{{inner: ::std::sync::Arc<H>}}
            impl<H> ::core::clone::Clone for ThriftService<H>{{fn clone(&self)->Self{{Self{{inner:self.inner.clone()}}}}}}
            impl<H:Handler> BoundService<H>{{pub fn thrift(&self)->ThriftService<H>{{ThriftService{{inner:self.inner.clone()}}}}}}
            impl<H:Handler> __thrift::server::NamedService for ThriftService<H>{{
                const NAME:&'static str={service_name:?};const METHODS:&'static [&'static str]=&[{method_names}];
                fn streaming_router(&self)->::std::option::Option<__thrift::transport::ttstream::connection::StreamRouter>{{
                    let mut router=__thrift::transport::ttstream::connection::StreamRouter::new();{registrations} ::std::option::Option::Some(router)
                }}
            }}
            impl<H:Handler> __volo::Service<__thrift::context::ServerContext,ThriftRequest> for ThriftService<H>{{
                type Response=ThriftResponse;type Error=__thrift::ServerError;
                async fn call(&self,cx:&mut __thrift::context::ServerContext,request:ThriftRequest)->::std::result::Result<Self::Response,Self::Error>{{match request{{{unary_arms}}}}}
            }}
        "#));
    }
    let grpc_registration = if variants.is_empty() {
        String::new()
    } else {
        "let registration=registration.with_grpc(self.grpc_route.clone().unwrap_or_else(||\
         __grpc::server::ServiceRoute::new(__grpc::server::ServiceBuilder::new(self.grpc()).build())));"
            .to_owned()
    };
    let stream_registration = if thrift {
        r#"let thrift = self.thrift();
           let router = __thrift::server::NamedService::streaming_router(&thrift);
           let registration = registration.with_thrift(thrift);
           let registration = match router {
               ::std::option::Option::Some(router) => registration.with_ttstream(router),
               ::std::option::Option::None => registration,
           };"#
    } else {
        ""
    };
    code.push_str(&format!(
        r#"
        impl<H:Handler> __runtime::Binding for BoundService<H>{{
            fn registration(&self)->__runtime::ServiceRegistration{{
                let registration=__runtime::ServiceRegistration::new(&DESCRIPTOR);
                {grpc_registration}
                {stream_registration}
                registration
            }}
        }}
    "#
    ));
    code.push('}');
    let code = remap_runtime_paths(&code)?;
    Ok((name, code))
}

fn remap_runtime_paths(code: &str) -> anyhow::Result<String> {
    use syn::visit_mut::VisitMut;
    struct Remap;
    impl VisitMut for Remap {
        fn visit_path_mut(&mut self, path: &mut syn::Path) {
            if path.leading_colon.is_some()
                && let Some(first) = path.segments.first_mut()
            {
                let alias = if first.ident == "volo_grpc" {
                    Some("__grpc")
                } else if first.ident == "volo_thrift" {
                    Some("__thrift")
                } else {
                    None
                };
                if let Some(alias) = alias {
                    first.ident = syn::Ident::new(alias, first.ident.span());
                    path.leading_colon = None;
                }
            }
            syn::visit_mut::visit_path_mut(self, path);
        }
    }
    let mut syntax = syn::parse_file(code)?;
    Remap.visit_file_mut(&mut syntax);
    Ok(quote::quote!(#syntax).to_string())
}

fn wrap_result(backend: &ThriftGrpcBackend, method: &Method, result: &str) -> String {
    if method.exceptions.is_some() {
        let arms = backend.exception_arms(method, result, true);
        format!("match value{{::std::result::Result::Ok(value)=>{result}::Ok(value),{arms}}}")
    } else {
        format!("{result}::Ok(value)")
    }
}

fn protobuf_entries(
    paths: &[String],
    variants: &[String],
    requests: &[String],
    replies: &[String],
) -> String {
    let inputs = variants
        .iter()
        .zip(requests)
        .map(|(v, t)| format!("{v}(__grpc::RecvStream<{t}>),"))
        .join("");
    let outputs = variants
        .iter()
        .zip(replies)
        .map(|(v, t)| format!("{v}(__grpc::streaming::response::ProtobufStream<{t}>),"))
        .join("");
    let decode = paths
        .iter()
        .zip(variants)
        .map(|(p, v)| {
            format!(
                "::std::option::Option::Some({p:?\
                 })=>::std::result::Result::Ok(Self::{v}(__grpc::RecvStream::new(body,kind,\
                 compression).with_limits(options.limits).with_context(options.context))),"
            )
        })
        .join("");
    let encode = variants
        .iter()
        .map(|v| {
            format!(
                "Self::{v}(messages)=>__grpc::SendEntryMessage::into_body_with_options(messages,\
                 compression,options),"
            )
        })
        .join("");
    format!(
        r#"
        pub enum H2Request{{{inputs}}}
        impl __grpc::RecvEntryMessage for H2Request{{
            fn from_body(method: ::std::option::Option<&str>,body:__grpc::body::BoxBody,kind:__grpc::codec::decode::Kind,compression: ::std::option::Option<__grpc::codec::compression::CompressionEncoding>)->::std::result::Result<Self,__grpc::Status>{{Self::from_body_with_options(method,body,kind,compression,__grpc::message::CodecOptions::new(__grpc::codec::CodecLimits::legacy()))}}
            fn from_body_with_options(method: ::std::option::Option<&str>,body:__grpc::body::BoxBody,kind:__grpc::codec::decode::Kind,compression: ::std::option::Option<__grpc::codec::compression::CompressionEncoding>,options:__grpc::message::CodecOptions)->::std::result::Result<Self,__grpc::Status>{{match method{{{decode}_=>::std::result::Result::Err(__grpc::Status::unimplemented("unknown method"))}}}}
        }}
        pub enum H2Response{{{outputs}}}
        impl __grpc::SendEntryMessage for H2Response{{
            fn into_body(self,compression: ::std::option::Option<__grpc::codec::compression::CompressionEncoding>)->__grpc::BoxStream<'static,::std::result::Result<__grpc::codegen::Frame<__grpc::codegen::Bytes>,__grpc::Status>>{{self.into_body_with_options(compression,__grpc::message::CodecOptions::new(__grpc::codec::CodecLimits::legacy()))}}
            fn into_body_with_options(self,compression: ::std::option::Option<__grpc::codec::compression::CompressionEncoding>,options:__grpc::message::CodecOptions)->__grpc::BoxStream<'static,::std::result::Result<__grpc::codegen::Frame<__grpc::codegen::Bytes>,__grpc::Status>>{{match self{{{encode}}}}}
        }}
    "#
    )
}

#[cfg(test)]
mod runtime_path_tests {
    #[test]
    fn remapping_preserves_literals_and_doc_comments() {
        let code = r#"#[doc = "::volo_grpc:: ::volo_thrift::"]
            const TEXT: &str = "::volo_grpc:: ::volo_thrift::";
            type Request = ::volo_grpc::Request<()>;
            type Error = ::volo_thrift::ServerError;"#;
        let mapped = super::remap_runtime_paths(code).unwrap();
        assert!(mapped.contains("\"::volo_grpc:: ::volo_thrift::\""));
        assert!(mapped.contains("__grpc :: Request"));
        assert!(mapped.contains("__thrift :: ServerError"));
    }
}
