use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use itertools::Itertools;
use pilota_build::{
    codegen::thrift::DecodeHelper,
    db::RirDatabase,
    rir::{self, Method},
    tags::RustWrapperArc,
    CodegenBackend, Context, DefId, IdentName, Symbol, ThriftBackend,
};
use quote::format_ident;
use volo::FastStr;

use crate::util::{get_base_dir, write_file, write_item};

#[derive(Clone)]
pub struct VoloThriftBackend {
    inner: ThriftBackend,
}

impl VoloThriftBackend {
    fn codegen_service_anonymous_type(&self, stream: &mut String, def_id: DefId, base_dir: &Path) {
        let service_name = self.cx().rust_name(def_id);
        let methods = self.cx().service_methods(def_id);
        let methods_names = methods.iter().map(|m| &**m.name).collect::<Vec<_>>();
        let variant_names = methods
            .iter()
            .map(|m| rust_name(self.cx(), m.def_id))
            .collect::<Vec<_>>();
        let args_recv_names = methods
            .iter()
            .map(|m| self.method_args_path(&service_name, m, false))
            .collect_vec();

        let args_send_names = methods
            .iter()
            .map(|m| self.method_args_path(&service_name, m, true))
            .collect_vec();

        let result_recv_names = methods
            .iter()
            .map(|m| self.method_result_path(&service_name, m, true))
            .collect_vec();

        let result_send_names = methods
            .iter()
            .map(|m| self.method_result_path(&service_name, m, false))
            .collect_vec();

        let req_recv_name = format!("{service_name}RequestRecv");
        let req_send_name = format!("{service_name}RequestSend");
        let res_recv_name = format!("{service_name}ResponseRecv");
        let res_send_name = format!("{service_name}ResponseSend");

        let (req_recv_impl, req_send_impl) = {
            let mk_decode = |is_async: bool, is_send: bool| {
                let helper = DecodeHelper::new(is_async);
                let mut match_methods = String::new();
                let args_names = if is_send {
                    args_send_names.clone()
                } else {
                    args_recv_names.clone()
                };
                for (methods_names, variant_names, args_name) in itertools::multizip((
                    methods_names.iter(),
                    variant_names.iter(),
                    args_names.iter(),
                )) {
                    let decode_variants = helper.codegen_item_decode(args_name.clone());
                    match_methods.push_str(&format!(
                        "\"{methods_names}\" => {{ Self::{variant_names}({decode_variants}) }},"
                    ));
                }
                // let decode_variants = helper.codegen_item_decode(req_recv_name.clone().into());
                // let match_methods = crate::join_multi_strs!("", |methods_names, variant_names| ->
                // "\"{methods_names}\" => {{ Self::{variant_names}({decode_variants}) }},");

                format! {
                    r#"::std::result::Result::Ok(match &*msg_ident.name {{
                        {match_methods}
                        _ => {{
                            return ::std::result::Result::Err(::pilota::thrift::new_application_exception(::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,  format!("unknown method {{}}", msg_ident.name)));
                        }},
                    }})"#
                }
            };

            let send_decode = mk_decode(false, true);
            let send_decode_async = mk_decode(true, true);
            let recv_decode = mk_decode(false, false);
            let recv_decode_async = mk_decode(true, false);

            let mut match_encode = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())}}");
            let mut match_size = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::volo_thrift::Message::size(value, __protocol)}}");

            if variant_names.is_empty() {
                match_encode = "_ => unreachable!(),".to_string();
                match_size = "_ => unreachable!(),".to_string();
            }

            let recv_impl = format! {
                r#"impl ::volo_thrift::EntryMessage for {req_recv_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, __protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(__protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {{
                       {recv_decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        __protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier
                    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException>
                        {{
                            {recv_decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}"#
            };

            let send_impl = format! {
                r#"impl ::volo_thrift::EntryMessage for {req_send_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, __protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(__protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {{
                       {send_decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        __protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier
                    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException>
                        {{
                            {send_decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}"#
            };

            (recv_impl, send_impl)
        };

        let (res_recv_impl, res_send_impl) = {
            let mk_decode = |is_async: bool, is_send: bool| {
                let helper = DecodeHelper::new(is_async);
                let mut match_methods = String::new();
                let args_names = if is_send {
                    result_send_names.clone()
                } else {
                    result_recv_names.clone()
                };
                for (methods_names, variant_names, args_name) in itertools::multizip((
                    methods_names.iter(),
                    variant_names.iter(),
                    args_names.iter(),
                )) {
                    let decode_item = helper.codegen_item_decode(args_name.clone());
                    match_methods.push_str(&format!(
                        "\"{methods_names}\" => {{ Self::{variant_names}({decode_item}) }},"
                    ));
                }
                // let decode_item = helper.codegen_item_decode(res_recv_name.clone().into());

                // let match_methods = crate::join_multi_strs!("", |methods_names, variant_names| ->
                // "\"{methods_names}\" => {{ Self::{variant_names}({decode_item}) }},");

                format!(
                    r#"::std::result::Result::Ok(match &*msg_ident.name {{
                        {match_methods}
                        _ => {{
                            return ::std::result::Result::Err(::pilota::thrift::new_application_exception(::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,  format!("unknown method {{}}", msg_ident.name)));
                        }},
                    }})"#
                )
            };

            let mut match_encode = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())}}");
            let mut match_size = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::volo_thrift::Message::size(value, __protocol)}}");

            if variant_names.is_empty() {
                match_encode = "_ => unreachable!(),".to_string();
                match_size = "_ => unreachable!(),".to_string();
            }

            let send_decode = mk_decode(false, true);
            let send_decode_async = mk_decode(true, true);
            let recv_decode = mk_decode(false, false);
            let recv_decode_async = mk_decode(true, false);

            let recv_impl = format! {
                r#"impl ::volo_thrift::EntryMessage for {res_recv_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, __protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(__protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {{
                       {recv_decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        __protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier,
                    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException>
                        {{
                            {recv_decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}"#
            };

            let send_impl = format! {
                r#"impl ::volo_thrift::EntryMessage for {res_send_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, __protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(__protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {{
                       {send_decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        __protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier,
                    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException>
                        {{
                            {send_decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}"#
            };

            (recv_impl, send_impl)
        };
        let req_recv_variants = crate::join_multi_strs!(
            ",",
            |variant_names, args_recv_names| -> "{variant_names}({args_recv_names})"
        );

        let req_send_variants = crate::join_multi_strs!(
            ",",
            |variant_names, args_send_names| -> "{variant_names}({args_send_names})"
        );

        let res_recv_variants = crate::join_multi_strs!(
            ",",
            |variant_names, result_recv_names| -> "{variant_names}({result_recv_names})"
        );
        let res_send_variants = crate::join_multi_strs!(
            ",",
            |variant_names, result_send_names| -> "{variant_names}({result_send_names})"
        );

        if self.cx().split {
            let req_recv_stream = format! {
                r#"#[derive(Debug, Clone)]
                pub enum {req_recv_name} {{
                    {req_recv_variants}
                }}

                {req_recv_impl}
            "#
            };

            let req_send_stream = format! {
                r#"#[derive(Debug, Clone)]
            pub enum {req_send_name} {{
                {req_send_variants}
            }}

            {req_send_impl}
            "#
            };

            let res_recv_stream = format! {
                r#"#[derive(Debug, Clone)]
            pub enum {res_recv_name} {{
                {res_recv_variants}
            }}
            {res_recv_impl}
            "#
            };

            let res_send_stream = format! {
                r#"#[derive(Debug, Clone)]
            pub enum {res_send_name} {{
                {res_send_variants}
            }}

            {res_send_impl}
            "#
            };

            write_item(
                stream,
                base_dir,
                format!("enum_{}.rs", &req_recv_name),
                req_recv_stream,
            );
            write_item(
                stream,
                base_dir,
                format!("enum_{}.rs", &res_recv_name),
                res_recv_stream,
            );
            write_item(
                stream,
                base_dir,
                format!("enum_{}.rs", &req_send_name),
                req_send_stream,
            );
            write_item(
                stream,
                base_dir,
                format!("enum_{}.rs", &res_send_name),
                res_send_stream,
            );
        } else {
            stream.push_str(&format! {
                r#"#[derive(Debug, Clone)]
            pub enum {req_recv_name} {{
                {req_recv_variants}
            }}

            #[derive(Debug, Clone)]
            pub enum {req_send_name} {{
                {req_send_variants}
            }}

            #[derive(Debug, Clone)]
            pub enum {res_recv_name} {{
                {res_recv_variants}
            }}

            #[derive(Debug, Clone)]
            pub enum {res_send_name} {{
                {res_send_variants}
            }}

            {req_recv_impl}
            {req_send_impl}
            {res_recv_impl}
            {res_send_impl}
            "#
            });
        }
    }

    fn method_ty_path(&self, service_name: &Symbol, method: &Method, suffix: &str) -> FastStr {
        match method.source {
            rir::MethodSource::Extend(def_id) => {
                let item = self.cx().expect_item(def_id);
                let target_service = match &*item {
                    rir::Item::Service(s) => s,
                    _ => panic!("expected service"),
                };
                let ident = &*format!(
                    "{}{}{}",
                    target_service.name,
                    rust_name(self.cx(), method.def_id),
                    suffix,
                );

                let path = self.cx().cur_related_item_path(def_id);
                let mut path = path.split("::").collect_vec();
                path.pop();
                path.push(ident);
                let path = path.join("::");
                path.into()
            }
            rir::MethodSource::Own => format!(
                "{}{}{}",
                service_name,
                rust_name(self.cx(), method.def_id),
                suffix
            )
            .into(),
        }
    }

    fn method_args_path(&self, service_name: &Symbol, method: &Method, is_client: bool) -> FastStr {
        if is_client {
            self.method_ty_path(service_name, method, "ArgsSend")
        } else {
            self.method_ty_path(service_name, method, "ArgsRecv")
        }
    }

    fn method_result_path(
        &self,
        service_name: &Symbol,
        method: &Method,
        is_client: bool,
    ) -> FastStr {
        if is_client {
            self.method_ty_path(service_name, method, "ResultRecv")
        } else {
            self.method_ty_path(service_name, method, "ResultSend")
        }
    }
}

impl pilota_build::CodegenBackend for VoloThriftBackend {
    const PROTOCOL: &'static str = "thrift";

    fn codegen_struct_impl(&self, def_id: DefId, stream: &mut String, s: &rir::Message) {
        self.inner.codegen_struct_impl(def_id, stream, s)
    }

    fn codegen_service_impl(&self, def_id: DefId, stream: &mut String, _s: &rir::Service) {
        let service_name = self.cx().rust_name(def_id);
        let server_name = format!("{service_name}Server");
        let generic_client_name = format!("{service_name}GenericClient");
        let client_name = format!("{service_name}Client");
        let oneshot_client_name = format!("{service_name}OneShotClient");
        let client_builder_name = format!("{client_name}Builder");
        let req_send_name = format!("{service_name}RequestSend");
        let req_recv_name = format!("{service_name}RequestRecv");
        let res_send_name = format!("{service_name}ResponseSend");
        let res_recv_name = format!("{service_name}ResponseRecv");

        let all_methods = self.cx().service_methods(def_id);

        let mut client_methods = Vec::new();
        let mut oneshot_client_methods = Vec::new();

        let path = self.cx().item_path(def_id);
        let path = path.as_ref();
        let buf = get_base_dir(self.cx().mode.as_ref(), self.cx().names.get(&def_id), path);
        let base_dir = buf.as_path();

        if self.cx().split {
            std::fs::create_dir_all(base_dir).expect("Failed to create base directory");
        }

        all_methods.iter().for_each(|m| {
            let name = self.cx().rust_name(m.def_id);
            let resp_type = self.cx().codegen_item_ty(m.ret.kind.clone());
            let req_fields = m.args.iter().map(|a| {
                let name = self.cx().rust_name(a.def_id); // use the rust name as string format which will escape the keyword
                let ty = self.cx().codegen_item_ty(a.ty.kind.clone());
                let mut ty = format!("{ty}");
                if let Some(RustWrapperArc(true)) = self.cx().tags(a.tags_id).as_ref().and_then(|tags| tags.get::<RustWrapperArc>()) {
                    ty = format!("::std::sync::Arc<{ty}>");
                }
                if a.kind == rir::FieldKind::Optional{
                    ty = format!("::std::option::Option<{ty}>")
                };
                format!(", {name}: {ty}")
            }).join("");
            let method_name_str = &**m.name;
            let enum_variant = rust_name(self.cx(), m.def_id);
            let result_path = self.method_result_path(&service_name, m, true);
            let oneway = m.oneway;
            let none = if m.oneway {
                "None => { ::std::result::Result::Ok(()) }"
            } else {
                "None => unreachable!()"
            };
            let req_field_names = m.args.iter().map(|a| self.cx().rust_name(a.def_id)).join(","); // use the rust name as string format which will escape the keyword
            let anonymous_args_send_name = self.method_args_path(&service_name, m, true);
            let exception = if let Some(p) = &m.exceptions {
                self.cx().cur_related_item_path(p.did)
            } else {
                // only placeholder, should never be used
                "std::convert::Infallible".into()
            };

            let convert_exceptions = m.exceptions.iter().map(|p| {
                self.cx().expect_item(p.did)
            }).flat_map(|e| {
                match &*e {
                    rir::Item::Enum(e) => e.variants.iter().map(|v| {
                        let name = self.cx().rust_name(v.did);
                        format!("Some({res_recv_name}::{enum_variant}({result_path}::{name}(ex))) => ::std::result::Result::Ok(::volo_thrift::MaybeException::Exception({exception}::{name}(ex))),")
                    }).collect::<Vec<_>>(),
                    _ => panic!()
                }
            }).join("");

            let mut resp_type_str = format!("{resp_type}");
            let mut resp_str = "::std::result::Result::Ok(resp)";
            if !convert_exceptions.is_empty() {
                resp_type_str = format!("::volo_thrift::MaybeException<{resp_type_str}, {exception}>");
                resp_str = "::std::result::Result::Ok(::volo_thrift::MaybeException::Ok(resp))";
            }
            client_methods.push(format! {
                r#"pub async fn {name}(&self {req_fields}) -> ::std::result::Result<{resp_type_str}, ::volo_thrift::ClientError> {{
                    let req = {req_send_name}::{enum_variant}({anonymous_args_send_name} {{
                        {req_field_names}
                    }});
                    let mut cx = self.0.make_cx("{method_name_str}", {oneway});
                    #[allow(unreachable_patterns)]
                    let resp = match ::volo::service::Service::call(&self.0, &mut cx, req).await? {{
                        Some({res_recv_name}::{enum_variant}({result_path}::Ok(resp))) => {resp_str},{convert_exceptions}
                        {none},
                        _ => unreachable!()
                    }};
                    ::volo_thrift::context::CLIENT_CONTEXT_CACHE.with(|cache| {{
                        let mut cache = cache.borrow_mut();
                        if cache.len() < cache.capacity() {{
                            cache.push(cx);
                        }}
                    }});
                    resp
                }}"#
            });

            oneshot_client_methods.push(format! {
                r#"pub async fn {name}(self {req_fields}) -> ::std::result::Result<{resp_type_str}, ::volo_thrift::ClientError> {{
                    let req = {req_send_name}::{enum_variant}({anonymous_args_send_name} {{
                        {req_field_names}
                    }});
                    let mut cx = self.0.make_cx("{method_name_str}", {oneway});
                    #[allow(unreachable_patterns)]
                    let resp = match ::volo::client::OneShotService::call(self.0, &mut cx, req).await? {{
                        Some({res_recv_name}::{enum_variant}({result_path}::Ok(resp))) => {resp_str},{convert_exceptions}
                        {none},
                        _ => unreachable!()
                    }};
                    ::volo_thrift::context::CLIENT_CONTEXT_CACHE.with(|cache| {{
                        let mut cache = cache.borrow_mut();
                        if cache.len() < cache.capacity() {{
                            cache.push(cx);
                        }}
                    }});
                    resp
                }}"#
            });
        });

        let variants = all_methods
            .iter()
            .map(|m| rust_name(self.cx(), m.def_id))
            .collect_vec();

        let user_handler = all_methods
            .iter()
            .map(|m| {
                let name = self.cx().rust_name(m.def_id);
                let args = m
                    .args
                    .iter()
                    .map(|a| format!("args.{}", self.cx().rust_name(a.def_id))) // use the rust name as string format which will escape the keyword
                    .join(",");

                let has_exception = m.exceptions.is_some();
                let method_result_path = self.method_result_path(&service_name, m, false);

                let convert_exceptions = m
                    .exceptions
                    .iter()
                    .map(|p| self.cx().expect_item(p.did))
                    .flat_map(|e| match &*e {
                        rir::Item::Enum(e) => e
                            .variants
                            .iter()
                            .map(|v| {
                                let name = self.cx().rust_name(v.did);
                                let exception = self.cx().cur_related_item_path(m.exceptions.as_ref().expect("must be exception here").did);
                                format!(
                                "::std::result::Result::Ok(::volo_thrift::MaybeException::Exception({exception}::{name}(ex))) => {method_result_path}::{name}(ex),"
                            )
                            })
                            .collect::<Vec<_>>(),
                        _ => panic!(),
                    })
                    .join("");

                if has_exception {
                    format! {
                        r#"match self.inner.{name}({args}).await {{
                        ::std::result::Result::Ok(::volo_thrift::MaybeException::Ok(resp)) => {method_result_path}::Ok(resp),
                        {convert_exceptions}
                        ::std::result::Result::Err(err) => return ::std::result::Result::Err(err),
                    }}"#
                    }
                } else {
                    format! {
                        r#"match self.inner.{name}({args}).await {{
                        ::std::result::Result::Ok(resp) => {method_result_path}::Ok(resp),
                        ::std::result::Result::Err(err) => return ::std::result::Result::Err(err),
                    }}"#
                    }
                }
            })
            .collect_vec();

        let mk_client_name = format_ident!("Mk{}", generic_client_name);
        let client_methods = client_methods.join("\n");
        let oneshot_client_methods = oneshot_client_methods.join("\n");

        let handler = crate::join_multi_strs!("", |variants, user_handler| -> r#"{req_recv_name}::{variants}(args) => ::std::result::Result::Ok(
            {res_send_name}::{variants}(
                {user_handler}
            )),"#);

        let mut mod_rs_stream = String::new();

        let server_string = format! {
            r#"pub struct {server_name}<S> {{
                inner: S, // handler
            }}

            impl<S> {server_name}<S> where S: {service_name} + ::core::marker::Send + ::core::marker::Sync + 'static {{
                pub fn new(inner: S) -> ::volo_thrift::server::Server<Self, ::volo::layer::Identity, {req_recv_name}, ::volo_thrift::codec::default::DefaultMakeCodec<::volo_thrift::codec::default::ttheader::MakeTTHeaderCodec<::volo_thrift::codec::default::framed::MakeFramedCodec<::volo_thrift::codec::default::thrift::MakeThriftCodec>>>, ::volo_thrift::tracing::DefaultProvider> {{
                    ::volo_thrift::server::Server::new(Self {{
                        inner,
                    }})
                }}
            }}

            impl<T> ::volo::service::Service<::volo_thrift::context::ServerContext, {req_recv_name}> for {server_name}<T> where T: {service_name} + Send + Sync + 'static {{
                type Response = {res_send_name};
                type Error = ::volo_thrift::ServerError;

                async fn call<'s, 'cx>(&'s self, _cx: &'cx mut ::volo_thrift::context::ServerContext, req: {req_recv_name}) -> ::std::result::Result<Self::Response, Self::Error> {{
                    match req {{
                        {handler}
                    }}
                }}
            }}"#
        };

        let client_string = format! {
            r#" pub struct {mk_client_name};

            pub type {client_name} = {generic_client_name}<::volo::service::BoxCloneService<::volo_thrift::context::ClientContext, {req_send_name}, ::std::option::Option<{res_recv_name}>, ::volo_thrift::ClientError>>;

            impl<S> ::volo::client::MkClient<::volo_thrift::Client<S>> for {mk_client_name} {{
                type Target = {generic_client_name}<S>;
                fn mk_client(&self, service: ::volo_thrift::Client<S>) -> Self::Target {{
                    {generic_client_name}(service)
                }}
            }}

            #[derive(Clone)]
            pub struct {generic_client_name}<S>(pub ::volo_thrift::Client<S>);

            pub struct {oneshot_client_name}<S>(pub ::volo_thrift::Client<S>);

            impl<S: ::volo::service::Service<::volo_thrift::context::ClientContext, {req_send_name}, Response = ::std::option::Option<{res_recv_name}>, Error = ::volo_thrift::ClientError> + Send + Sync + 'static> {generic_client_name}<S> {{
                pub fn with_callopt<Opt: ::volo::client::Apply<::volo_thrift::context::ClientContext>>(self, opt: Opt) -> {oneshot_client_name}<::volo::client::WithOptService<S, Opt>> {{
                    {oneshot_client_name}(self.0.with_opt(opt))
                }}

                {client_methods}
            }}

            impl<S: ::volo::client::OneShotService<::volo_thrift::context::ClientContext, {req_send_name}, Response = ::std::option::Option<{res_recv_name}>, Error = ::volo_thrift::ClientError> + Send + Sync + 'static> {oneshot_client_name}<S> {{
                {oneshot_client_methods}
            }}

            pub struct {client_builder_name} {{
            }}

            impl {client_builder_name} {{
                pub fn new(service_name: impl AsRef<str>) -> ::volo_thrift::client::ClientBuilder<
                    ::volo::layer::Identity,
                    ::volo::layer::Identity,
                    {mk_client_name},
                    {req_send_name},
                    {res_recv_name},
                    ::volo::net::dial::DefaultMakeTransport,
                    ::volo_thrift::codec::default::DefaultMakeCodec<::volo_thrift::codec::default::ttheader::MakeTTHeaderCodec<::volo_thrift::codec::default::framed::MakeFramedCodec<::volo_thrift::codec::default::thrift::MakeThriftCodec>>>,
                    ::volo::loadbalance::LbConfig<::volo::loadbalance::random::WeightedRandomBalance<()>, ::volo::discovery::DummyDiscover>,
                >
                {{
                    ::volo_thrift::client::ClientBuilder::new(service_name, {mk_client_name})
                }}
            }}"#
        };

        if self.cx().split {
            write_item(
                &mut mod_rs_stream,
                base_dir,
                format!("service_{service_name}Server.rs"),
                server_string,
            );
            write_item(
                &mut mod_rs_stream,
                base_dir,
                format!("service_{service_name}Client.rs"),
                client_string,
            );
        } else {
            stream.push_str(&server_string);
            stream.push_str(&client_string);
        }

        if self.cx().split {
            self.codegen_service_anonymous_type(&mut mod_rs_stream, def_id, base_dir);
        } else {
            self.codegen_service_anonymous_type(stream, def_id, base_dir);
        }

        if self.cx().split {
            let mod_rs_file_path = base_dir.join("mod.rs");
            write_file(&mod_rs_file_path, mod_rs_stream);
            stream.push_str(
                format!(
                    "include!(\"{}/mod.rs\");",
                    base_dir.file_name().unwrap().to_str().unwrap()
                )
                .as_str(),
            );
        }
    }

    fn codegen_service_method(&self, _service_def_id: DefId, method: &Method) -> String {
        let name = self.cx().rust_name(method.def_id);
        let ret_ty = self.inner.codegen_item_ty(method.ret.kind.clone());
        let mut ret_ty = format!("{ret_ty}");
        if let Some(RustWrapperArc(true)) = self
            .cx()
            .tags(method.ret.tags_id)
            .as_ref()
            .and_then(|tags| tags.get::<RustWrapperArc>())
        {
            ret_ty = format!("::std::sync::Arc<{ret_ty}>");
        }
        let args = method
            .args
            .iter()
            .map(|a| {
                let ty = self.inner.codegen_item_ty(a.ty.kind.clone());
                let ident = self.cx().rust_name(a.def_id); // use the rust name as string format which will escape the keyword
                let ty = if let Some(RustWrapperArc(true)) = self
                    .cx()
                    .tags(a.tags_id)
                    .as_ref()
                    .and_then(|tags| tags.get::<RustWrapperArc>())
                {
                    format!("::std::sync::Arc<{ty}>")
                } else {
                    ty.to_string()
                };
                let ty = if a.kind == rir::FieldKind::Optional {
                    format!("::std::option::Option<{ty}>")
                } else {
                    ty.to_string()
                };
                format!("{ident}: {ty}")
            })
            .join(",");

        if let Some(p) = &method.exceptions {
            let exception = self.inner.cur_related_item_path(p.did);
            ret_ty = format!("::volo_thrift::MaybeException<{ret_ty}, {exception}>");
        }

        format!(
            "fn {name}(&self, {args}) -> impl ::std::future::Future<Output = \
             ::core::result::Result<{ret_ty}, ::volo_thrift::ServerError>> + Send;"
        )
    }

    fn codegen_service_method_with_global_path(
        &self,
        _service_def_id: DefId,
        method: &Method,
    ) -> String {
        let name = self.cx().rust_name(method.def_id);
        let mut ret_ty = self
            .inner
            .codegen_item_ty(method.ret.kind.clone())
            .global_path("volo_gen")
            .to_string();
        if let Some(RustWrapperArc(true)) = self
            .cx()
            .tags(method.ret.tags_id)
            .as_ref()
            .and_then(|tags| tags.get::<RustWrapperArc>())
        {
            ret_ty = format!("::std::sync::Arc<{ret_ty}>");
        }
        let args = method
            .args
            .iter()
            .map(|a| {
                let ty = self
                    .inner
                    .codegen_item_ty(a.ty.kind.clone())
                    .global_path("volo_gen");
                let ident = self.cx().rust_name(a.def_id).0.field_ident(); // use the _{rust-style fieldname} without keyword escaping
                format!("_{ident}: {ty}")
            })
            .join(",");

        if let Some(p) = &method.exceptions {
            let exception = self.inner.cur_related_item_path(p.did);
            ret_ty = format!("::volo_thrift::MaybeException<{ret_ty}, {exception}>");
        }

        format!(
            r#"async fn {name}(&self, {args}) -> ::core::result::Result<{ret_ty}, ::volo_thrift::ServerError>
            {{
                ::std::result::Result::Ok(Default::default())
            }}"#
        )
    }

    fn codegen_enum_impl(&self, def_id: DefId, stream: &mut String, e: &rir::Enum) {
        self.inner.codegen_enum_impl(def_id, stream, e)
    }

    fn codegen_newtype_impl(&self, def_id: DefId, stream: &mut String, t: &rir::NewType) {
        self.inner.codegen_newtype_impl(def_id, stream, t)
    }

    fn codegen_file_descriptor(&self, stream: &mut String, f: &rir::File, has_direct: bool) {
        self.inner.codegen_file_descriptor(stream, f, has_direct)
    }
    fn codegen_register_mod_file_descriptor(
        &self,
        stream: &mut String,
        mods: &[(Arc<[Symbol]>, Arc<PathBuf>)],
    ) {
        self.inner
            .codegen_register_mod_file_descriptor(stream, mods)
    }

    fn cx(&self) -> &Context {
        self.inner.cx()
    }
}

fn rust_name(cx: &Context, def_id: DefId) -> FastStr {
    let name = cx.rust_name(def_id);
    if cx.names.contains_key(&def_id) {
        name.0
    } else {
        name.0.upper_camel_ident()
    }
}

pub struct MkThriftBackend;

impl pilota_build::MakeBackend for MkThriftBackend {
    type Target = VoloThriftBackend;

    fn make_backend(self, context: Context) -> Self::Target {
        VoloThriftBackend {
            inner: ThriftBackend::new(context),
        }
    }
}
