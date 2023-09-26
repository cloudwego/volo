use itertools::Itertools;
use pilota_build::{
    codegen::thrift::DecodeHelper, db::RirDatabase, rir, rir::Method, tags::RustWrapperArc,
    CodegenBackend, Context, DefId, IdentName, Symbol, ThriftBackend,
};
use quote::format_ident;
use volo::FastStr;

#[derive(Clone)]
pub struct VoloThriftBackend {
    inner: ThriftBackend,
}

impl VoloThriftBackend {
    fn codegen_service_anonymous_type(&self, stream: &mut String, def_id: DefId) {
        let service_name = self.cx().rust_name(def_id);
        let methods = self.cx().service_methods(def_id);
        let methods_names = methods.iter().map(|m| &**m.name).collect::<Vec<_>>();
        let variant_names = methods
            .iter()
            .map(|m| self.cx().rust_name(m.def_id).0.upper_camel_ident())
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

        let req_impl = {
            let mk_decode = |is_async: bool| {
                let helper = DecodeHelper::new(is_async);
                let decode_variants = helper.codegen_item_decode();
                let match_methods = crate::join_multi_strs!("", |methods_names, variant_names| -> "\"{methods_names}\" => {{ Self::{variant_names}({decode_variants}) }},");

                format! {
                    r#"Ok(match &*msg_ident.name {{
                        {match_methods}
                        _ => {{
                            return Err(::pilota::thrift::DecodeError::new(::pilota::thrift::DecodeErrorKind::UnknownMethod,  format!("unknown method {{}}", msg_ident.name)));
                        }},
                    }})"#
                }
            };

            let decode = mk_decode(false);
            let decode_async = mk_decode(true);

            let mut match_encode = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::pilota::thrift::Message::encode(value, protocol).map_err(|err| err.into())}}");
            let mut match_size = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::volo_thrift::Message::size(value, protocol)}}");

            if variant_names.is_empty() {
                match_encode = "_ => unreachable!(),".to_string();
                match_size = "_ => unreachable!(),".to_string();
            }

            format! {
                r#"#[::async_trait::async_trait]
                impl ::volo_thrift::EntryMessage for {req_recv_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::EncodeError> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError> {{
                       {decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier
                    ) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError>
                        {{
                            {decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}

                #[::async_trait::async_trait]
                impl ::volo_thrift::EntryMessage for {req_send_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::EncodeError> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError> {{
                       {decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier
                    ) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError>
                        {{
                            {decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}"#
            }
        };

        let res_impl = {
            let mk_decode = |is_async: bool| {
                let helper = DecodeHelper::new(is_async);
                let decode_item = helper.codegen_item_decode();

                let match_methods = crate::join_multi_strs!("", |methods_names, variant_names| -> "\"{methods_names}\" => {{ Self::{variant_names}({decode_item}) }},");

                format!(
                    r#"Ok(match &*msg_ident.name {{
                        {match_methods}
                        _ => {{
                            return Err(::pilota::thrift::DecodeError::new(::pilota::thrift::DecodeErrorKind::UnknownMethod,  format!("unknown method {{}}", msg_ident.name)));
                        }},
                    }})"#
                )
            };

            let mut match_encode = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::pilota::thrift::Message::encode(value, protocol).map_err(|err| err.into())}}");
            let mut match_size = crate::join_multi_strs!(",", |variant_names| -> "Self::{variant_names}(value) => {{::volo_thrift::Message::size(value, protocol)}}");

            if variant_names.is_empty() {
                match_encode = "_ => unreachable!(),".to_string();
                match_size = "_ => unreachable!(),".to_string();
            }

            let decode = mk_decode(false);
            let decode_async = mk_decode(true);
            format! {
                r#"#[::async_trait::async_trait]
                impl ::volo_thrift::EntryMessage for {res_recv_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::EncodeError> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError> {{
                       {decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier,
                    ) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError>
                        {{
                            {decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}

                #[::async_trait::async_trait]
                impl ::volo_thrift::EntryMessage for {res_send_name} {{
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, protocol: &mut T) -> ::core::result::Result<(), ::pilota::thrift::EncodeError> {{
                        match self {{
                            {match_encode}
                        }}
                    }}

                    fn decode<T: ::pilota::thrift::TInputProtocol>(protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError> {{
                       {decode}
                    }}

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier,
                    ) -> ::core::result::Result<Self, ::pilota::thrift::DecodeError>
                        {{
                            {decode_async}
                        }}

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {{
                        match self {{
                            {match_size}
                        }}
                    }}
                }}"#
            }
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

            {req_impl}
            {res_impl}"#
        });
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
                    self.cx().rust_name(method.def_id).0.upper_camel_ident(),
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
                self.cx().rust_name(method.def_id).0.upper_camel_ident(),
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

        all_methods.iter().for_each(|m| {
            let name = self.cx().rust_name(m.def_id);
            let resp_type = self.cx().codegen_item_ty(m.ret.kind.clone());
            let req_fields = m.args.iter().map(|a| {
                let name = self.cx().rust_name(a.def_id);
                let ty = self.cx().codegen_item_ty(a.ty.kind.clone());
                let mut ty = format!("{ty}");
                if let Some(RustWrapperArc(true)) = self.cx().tags(a.tags_id).as_ref().and_then(|tags| tags.get::<RustWrapperArc>()) {
                    ty = format!("::std::sync::Arc<{ty}>");
                }
                format!(", {name}: {ty}")
            }).join("");
            let method_name_str = &**m.name;
            let enum_variant = self.cx().rust_name(m.def_id).0.upper_camel_ident();
            let result_path = self.method_result_path(&service_name, m, true);
            let oneway = m.oneway;
            let none = if m.oneway {
                "None => { Ok(()) }"
            } else {
                "None => unreachable!()"
            };
            let req_field_names = m.args.iter().map(|a| self.cx().rust_name(a.def_id)).join(",");
            let anonymous_args_send_name = self.method_args_path(&service_name, m, true);
            let exception = if let Some(p) = &m.exceptions {
                self.cx().cur_related_item_path(p.did)
            } else {
                "std::convert::Infallible".into()
            };

            let convert_exceptions = m.exceptions.iter().map(|p| {
                self.cx().expect_item(p.did)
            }).flat_map(|e| {
                match &*e {
                    rir::Item::Enum(e) => e.variants.iter().map(|v| {
                        let name = self.cx().rust_name(v.did);
                        format!("Some({res_recv_name}::{enum_variant}({result_path}::{name}(err))) => Err(::volo_thrift::error::ResponseError::UserException({exception}::{name}(err))),")
                    }).collect::<Vec<_>>(),
                    _ => panic!()
                }
            }).join("");

            client_methods.push(format! {
                r#"pub async fn {name}(&self {req_fields}) -> ::std::result::Result<{resp_type}, ::volo_thrift::error::ResponseError<{exception}>> {{
                    let req = {req_send_name}::{enum_variant}({anonymous_args_send_name} {{
                        {req_field_names}
                    }});
                    let mut cx = self.0.make_cx("{method_name_str}", {oneway});
                    #[allow(unreachable_patterns)]
                    let resp = match ::volo::service::Service::call(&self.0, &mut cx, req).await? {{
                        Some({res_recv_name}::{enum_variant}({result_path}::Ok(resp))) => Ok(resp),{convert_exceptions}
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
                r#"pub async fn {name}(self {req_fields}) -> ::std::result::Result<{resp_type}, ::volo_thrift::error::ResponseError<{exception}>> {{
                    let req = {req_send_name}::{enum_variant}({anonymous_args_send_name} {{
                        {req_field_names}
                    }});
                    let mut cx = self.0.make_cx("{method_name_str}", {oneway});
                    #[allow(unreachable_patterns)]
                    let resp = match ::volo::client::OneShotService::call(self.0, &mut cx, req).await? {{
                        Some({res_recv_name}::{enum_variant}({result_path}::Ok(resp))) => Ok(resp),{convert_exceptions}
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
            .map(|m| self.cx().rust_name(m.def_id).0.upper_camel_ident())
            .collect_vec();

        let user_handler = all_methods
            .iter()
            .map(|m| {
                let name = self.cx().rust_name(m.def_id);
                let args = m
                    .args
                    .iter()
                    .map(|a| format!("args.{}", self.cx().rust_name(a.def_id)))
                    .join(",");

                let has_exception = m.exceptions.is_some();
                let method_result_path = self.method_result_path(&service_name, m, false);

                let exception: FastStr = if let Some(p) = &m.exceptions {
                    self.cx().cur_related_item_path(p.did)
                } else {
                    "::volo_thrift::error::DummyError".into()
                };

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
                                format!(
                                "Err(::volo_thrift::error::UserError::UserException({exception}::{name}(err))) => {method_result_path}::{name}(err),"
                            )
                            })
                            .collect::<Vec<_>>(),
                        _ => panic!(),
                    })
                    .join("");

                if has_exception {
                    format! {
                        r#"match self.inner.{name}({args}).await {{
                        Ok(resp) => {method_result_path}::Ok(resp),
                        {convert_exceptions}
                        Err(::volo_thrift::error::UserError::Other(err)) => return Err(err),
                    }}"#
                    }
                } else {
                    format! {
                        r#"match self.inner.{name}({args}).await {{
                        Ok(resp) => {method_result_path}::Ok(resp),
                        Err(err) => return Err(err),
                    }}"#
                    }
                }
            })
            .collect_vec();

        let mk_client_name = format_ident!("Mk{}", generic_client_name);
        let client_methods = client_methods.join("\n");
        let oneshot_client_methods = oneshot_client_methods.join("\n");

        let handler = crate::join_multi_strs!("", |variants, user_handler| -> r#"{req_recv_name}::{variants}(args) => Ok(
            {res_send_name}::{variants}(
                {user_handler}
            )),"#);

        stream.push_str(&format! {
            r#"pub struct {server_name}<S> {{
                inner: S, // handler
            }}

            pub struct {mk_client_name};

            pub type {client_name} = {generic_client_name}<::volo::service::BoxCloneService<::volo_thrift::context::ClientContext, {req_send_name}, ::std::option::Option<{res_recv_name}>, ::volo_thrift::Error>>;

            impl<S> ::volo::client::MkClient<::volo_thrift::Client<S>> for {mk_client_name} {{
                type Target = {generic_client_name}<S>;
                fn mk_client(&self, service: ::volo_thrift::Client<S>) -> Self::Target {{
                    {generic_client_name}(service)
                }}
            }}

            #[derive(Clone)]
            pub struct {generic_client_name}<S>(pub ::volo_thrift::Client<S>);

            pub struct {oneshot_client_name}<S>(pub ::volo_thrift::Client<S>);

            impl<S: ::volo::service::Service<::volo_thrift::context::ClientContext, {req_send_name}, Response = ::std::option::Option<{res_recv_name}>, Error = ::volo_thrift::Error> + Send + Sync + 'static> {generic_client_name}<S> {{
                pub fn with_callopt<Opt: ::volo::client::Apply<::volo_thrift::context::ClientContext>>(self, opt: Opt) -> {oneshot_client_name}<::volo::client::WithOptService<S, Opt>> {{
                    {oneshot_client_name}(self.0.with_opt(opt))
                }}

                {client_methods}
            }}

            impl<S: ::volo::client::OneShotService<::volo_thrift::context::ClientContext, {req_send_name}, Response = ::std::option::Option<{res_recv_name}>, Error = ::volo_thrift::Error> + Send + Sync + 'static> {oneshot_client_name}<S> {{
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
                type Error = ::anyhow::Error;

                type Future<'cx> = impl ::std::future::Future<Output = ::std::result::Result<Self::Response, Self::Error>> + 'cx;

                fn call<'cx, 's>(&'s self, _cx: &'cx mut ::volo_thrift::context::ServerContext, req: {req_recv_name}) -> Self::Future<'cx> where 's:'cx {{
                    async move {{
                        match req {{
                           {handler}
                        }}
                    }}
                }}
            }}"#
        });
        self.codegen_service_anonymous_type(stream, def_id);
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
                let ident = self.cx().rust_name(a.def_id);
                format!("{ident}: {ty}")
            })
            .join(",");

        let exception: FastStr = if let Some(p) = &method.exceptions {
            let exception = self.inner.cur_related_item_path(p.did);
            format! {"::volo_thrift::error::UserError<{exception}>" }.into()
        } else {
            "::volo_thrift::AnyhowError".into()
        };

        format!("async fn {name}(&self, {args}) -> ::core::result::Result<{ret_ty}, {exception}>;")
    }

    fn codegen_service_method_with_global_path(
        &self,
        _service_def_id: DefId,
        method: &Method,
    ) -> String {
        let name = self.cx().rust_name(method.def_id);
        let ret_ty = self
            .inner
            .codegen_item_ty(method.ret.kind.clone())
            .global_path();
        let mut ret_ty = format!("volo_gen{ret_ty}");
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
                let ty = self.inner.codegen_item_ty(a.ty.kind.clone()).global_path();
                let ident = self.cx().rust_name(a.def_id);
                format!("_{ident}: volo_gen{ty}")
            })
            .join(",");

        let exception: FastStr = if let Some(p) = &method.exceptions {
            let exception = self.inner.item_path(p.did).join("::");
            format! {"::volo_thrift::error::UserError<volo_gen::{exception}>" }.into()
        } else {
            "::volo_thrift::AnyhowError".into()
        };

        format!(
            r#"async fn {name}(&self, {args}) -> ::core::result::Result<{ret_ty}, {exception}>{{
					Ok(Default::default())
				}}"#
        )
    }

    fn codegen_enum_impl(&self, def_id: DefId, stream: &mut String, e: &rir::Enum) {
        self.inner.codegen_enum_impl(def_id, stream, e)
    }

    fn codegen_newtype_impl(&self, def_id: DefId, stream: &mut String, t: &rir::NewType) {
        self.inner.codegen_newtype_impl(def_id, stream, t)
    }

    fn cx(&self) -> &Context {
        self.inner.cx()
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
