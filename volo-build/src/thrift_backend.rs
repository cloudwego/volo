use std::sync::Arc;

use itertools::Itertools;
use pilota_build::{
    codegen::thrift::DecodeHelper, db::RirDatabase, rir, rir::Method, tags::RustWrapperArc,
    Context, DefId, IdentName, ThriftBackend,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

pub struct VoloThriftBackend {
    cx: Arc<Context>,
    inner: ThriftBackend,
}

impl VoloThriftBackend {
    fn codegen_service_anonymous_type(&self, stream: &mut TokenStream, def_id: DefId) {
        let service_name = self.cx.rust_name(def_id).as_syn_ident();
        let methods = self.cx.service_methods(def_id);
        let methods_names = methods.iter().map(|m| &**m.name).collect::<Vec<_>>();
        let variant_names = methods
            .iter()
            .map(|m| {
                self.cx
                    .rust_name(m.def_id)
                    .upper_camel_ident()
                    .as_syn_ident()
            })
            .collect::<Vec<_>>();
        let args_recv_names = methods
            .iter()
            .map(|m| self.method_args_path(&service_name, m, false));
        let args_send_names = methods
            .iter()
            .map(|m| self.method_args_path(&service_name, m, true));

        let result_names = methods
            .iter()
            .map(|m| self.method_result_path(&service_name, m));

        let req_recv_name = format_ident!("{}RequestRecv", service_name);
        let req_send_name = format_ident!("{}RequestSend", service_name);
        let res_name = format_ident!("{}Response", service_name);

        let req_impl = {
            let mk_decode = |is_async: bool| {
                let helper = DecodeHelper::new(is_async);
                let decode_variants = helper.codegen_item_decode();

                quote! {
                    Ok(match &*msg_ident.name {
                        #(#methods_names => {
                            Self::#variant_names(#decode_variants)
                        }),*
                        _ => {
                            return Err(::volo_thrift::error::new_application_error(::volo_thrift::error::ApplicationErrorKind::UnknownMethod,  format!("unknown method {}", msg_ident.name)));
                        },
                    })
                }
            };

            let decode = mk_decode(false);
            let decode_async = mk_decode(true);
            quote! {
                #[::async_trait::async_trait]
                impl ::volo_thrift::EntryMessage for #req_recv_name {
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, protocol: &mut T) -> ::core::result::Result<(), ::volo_thrift::Error> {
                        match self {
                            #(Self::#variant_names(value) => {
                                ::pilota::thrift::Message::encode(value, protocol).map_err(|err| err.into())
                            }),*
                        }
                    }

                    fn decode<T: ::pilota::thrift::TInputProtocol>(protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::volo_thrift::Error> {
                       #decode
                    }

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier
                    ) -> ::core::result::Result<Self, ::volo_thrift::Error>
                        {
                            #decode_async
                        }

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {
                        match self {
                            #(Self::#variant_names(value) => {
                                ::volo_thrift::Message::size(value, protocol)
                            }),*
                        }
                    }
                }

                #[::async_trait::async_trait]
                impl ::volo_thrift::EntryMessage for #req_send_name {
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, protocol: &mut T) -> ::core::result::Result<(), ::volo_thrift::Error> {
                        match self {
                            #(Self::#variant_names(value) => {
                                ::pilota::thrift::Message::encode(value, protocol).map_err(|err| err.into())
                            }),*
                        }
                    }

                    fn decode<T: ::pilota::thrift::TInputProtocol>(protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::volo_thrift::Error> {
                       #decode
                    }

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier
                    ) -> ::core::result::Result<Self, ::volo_thrift::Error>
                        {
                            #decode_async
                        }

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {
                        match self {
                            #(Self::#variant_names(value) => {
                                ::volo_thrift::Message::size(value, protocol)
                            }),*
                        }
                    }
                }
            }
        };

        let res_impl = {
            let mk_decode = |is_async: bool| {
                let helper = DecodeHelper::new(is_async);
                let decode_item = helper.codegen_item_decode();

                quote! {
                    let is_err = matches!(msg_ident.message_type, ::pilota::thrift::TMessageType::Exception);
                    Ok(match &*msg_ident.name {
                        #(#methods_names => {
                            Self::#variant_names(#decode_item)
                        }),*
                        _ => {
                            return Err(::volo_thrift::error::new_application_error(::volo_thrift::error::ApplicationErrorKind::UnknownMethod,  format!("unknown method {}", msg_ident.name)));
                        },
                    })
                }
            };

            let decode = mk_decode(false);
            let decode_async = mk_decode(true);
            quote! {
                #[::async_trait::async_trait]
                impl ::volo_thrift::EntryMessage for #res_name {
                    fn encode<T: ::pilota::thrift::TOutputProtocol>(&self, protocol: &mut T) -> ::core::result::Result<(), ::volo_thrift::Error> {
                        match self {
                            #(
                                Self::#variant_names(value) => {
                                    ::pilota::thrift::Message::encode(value, protocol).map_err(|err| err.into())
                                }
                            )*
                        }
                    }

                    fn decode<T: ::pilota::thrift::TInputProtocol>(protocol: &mut T, msg_ident: &::pilota::thrift::TMessageIdentifier) -> ::core::result::Result<Self, ::volo_thrift::Error> {
                       #decode
                    }

                    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
                        protocol: &mut T,
                        msg_ident: &::pilota::thrift::TMessageIdentifier,
                    ) -> ::core::result::Result<Self, ::volo_thrift::Error>
                        {
                            #decode_async
                        }

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &mut T) -> usize {
                        match self {
                            #(
                                Self::#variant_names(value) => {
                                    ::volo_thrift::Message::size(value, protocol)
                                }
                            )*
                        }
                    }
                }

            }
        };
        stream.extend(quote! {
            #[derive(Debug, Clone)]
            pub enum #req_recv_name {
                #(#variant_names(#args_recv_names)),*
            }

            #[derive(Debug, Clone)]
            pub enum #req_send_name {
                #(#variant_names(#args_send_names)),*
            }

            #[derive(Debug, Clone)]
            pub enum #res_name {
                #(#variant_names(#result_names)),*
            }

            #req_impl
            #res_impl
        });
    }

    fn method_ty_path(&self, service_name: &Ident, method: &Method, suffix: &str) -> TokenStream {
        match method.source {
            rir::MethodSource::Extend(def_id) => {
                let item = self.cx.expect_item(def_id);
                let target_service = match &*item {
                    rir::Item::Service(s) => s,
                    _ => panic!("expected service"),
                };
                let ident = (&*format!(
                    "{}{}{}",
                    target_service.name,
                    self.cx.rust_name(method.def_id).upper_camel_ident(),
                    suffix,
                ))
                    .as_syn_ident();
                let mut path: syn::Path = self.cx.cur_related_item_path(def_id);
                path.segments.pop();
                path.segments.push(ident.into());
                quote! { #path }
            }
            rir::MethodSource::Own => {
                let ident = (&*format!(
                    "{}{}{}",
                    service_name,
                    self.cx.rust_name(method.def_id).upper_camel_ident(),
                    suffix
                ))
                    .as_syn_ident();
                quote!(#ident)
            }
        }
    }

    fn method_args_path(
        &self,
        service_name: &Ident,
        method: &Method,
        is_client: bool,
    ) -> TokenStream {
        if is_client {
            self.method_ty_path(service_name, method, "ArgsSend")
        } else {
            self.method_ty_path(service_name, method, "ArgsRecv")
        }
    }

    fn method_result_path(&self, service_name: &Ident, method: &Method) -> TokenStream {
        self.method_ty_path(service_name, method, "Result")
    }
}

impl pilota_build::CodegenBackend for VoloThriftBackend {
    fn codegen_struct_impl(&self, def_id: DefId, stream: &mut TokenStream, s: &rir::Message) {
        self.inner.codegen_struct_impl(def_id, stream, s)
    }

    fn codegen_service_impl(&self, def_id: DefId, stream: &mut TokenStream, _s: &rir::Service) {
        let service_name = self.cx.rust_name(def_id).as_syn_ident();
        let server_name = format_ident!("{}Server", service_name);
        let generic_client_name = format_ident!("{}GenericClient", service_name);
        let client_name = format_ident!("{}Client", service_name);
        let oneshot_client_name = format_ident!("{}OneShotClient", service_name);
        let client_builder_name = format_ident!("{}Builder", client_name);
        let req_send_name = format_ident!("{}RequestSend", service_name);
        let req_recv_name = format_ident!("{}RequestRecv", service_name);
        let res_name = format_ident!("{}Response", service_name);

        let all_methods = self.cx.service_methods(def_id);

        let mut client_methods = Vec::new();
        let mut oneshot_client_methods = Vec::new();

        all_methods.iter().for_each(|m| {
            let name = self.cx.rust_name(m.def_id).as_syn_ident();
            let resp_type = self.cx.codegen_item_ty(m.ret.kind.clone());
            let req_fields = m.args.iter().map(|a| {
                let name = self.cx.rust_name(a.def_id).as_syn_ident();
                let ty = self.cx.codegen_item_ty(a.ty.kind.clone());
                let mut ty = quote! { #ty };
                if let Some(RustWrapperArc(true)) = self.cx.tags(a.tags_id).as_ref().and_then(|tags| tags.get::<RustWrapperArc>()) {
                    ty = quote! { ::std::sync::Arc<#ty> };
                }
                quote! {
                    #name: #ty
                }
            }).collect_vec();
            let method_name_str = &**m.name;
            let enum_variant = self.cx.rust_name(m.def_id).upper_camel_ident().as_syn_ident();
            let result_path = self.method_result_path(&service_name, m);
            let oneway = m.oneway;
            let none = if m.oneway {
                quote! {
                    None => { Ok(()) }
                }
            } else {
                quote! {
                    None => unreachable!()
                }
            };
            let req_field_names = m.args.iter().map(|a| self.cx.rust_name(a.def_id).as_syn_ident()).collect_vec();
            let anonymous_args_send_name = self.method_args_path(&service_name, m, true);
            let exception = if let Some(p) = &m.exceptions {
                let path = self.cx.cur_related_item_path(p.did);
                quote!{ #path }
            } else {
                quote!(std::convert::Infallible)
            };

            let convert_exceptions = m.exceptions.iter().map(|p| {
                self.cx.expect_item(p.did)
            }).flat_map(|e| {
                match &*e {
                    rir::Item::Enum(e) => e.variants.iter().map(|v| {
                        let name = self.cx.rust_name(v.did).as_syn_ident();
                        quote! {
                            Some(#res_name::#enum_variant(#result_path::#name(err))) => Err(::volo_thrift::error::ResponseError::UserException(#exception::#name(err)))
                        }
                    }).collect::<Vec<_>>(),
                    _ => panic!()
                }
            }).collect_vec();

            client_methods.push(quote! {
                pub async fn #name(&self #(, #req_fields)*) -> ::std::result::Result<#resp_type, ::volo_thrift::error::ResponseError<#exception>> {
                    let req = #req_send_name::#enum_variant(#anonymous_args_send_name {
                        #(#req_field_names),*
                    });
                    let mut cx = self.0.make_cx(#method_name_str, #oneway);
                    #[allow(unreachable_patterns)]
                    match ::volo::service::Service::call(&self.0, &mut cx, req).await? {
                        Some(#res_name::#enum_variant(#result_path::Ok(resp))) => Ok(resp),
                        #(#convert_exceptions,)*
                        #none,
                        _ => unreachable!()
                    }
                }
            });

            oneshot_client_methods.push(quote! {
                pub async fn #name(self #(, #req_fields)*) -> ::std::result::Result<#resp_type, ::volo_thrift::error::ResponseError<#exception>> {
                    let req = #req_send_name::#enum_variant(#anonymous_args_send_name {
                        #(#req_field_names),*
                    });
                    let mut cx = self.0.make_cx(#method_name_str, #oneway);
                    #[allow(unreachable_patterns)]
                    match ::volo::client::OneShotService::call(self.0, &mut cx, req).await? {
                        Some(#res_name::#enum_variant(#result_path::Ok(resp))) => Ok(resp),
                        #(#convert_exceptions,)*
                        #none,
                        _ => unreachable!()
                    }
                }
            });
        });

        let variants = all_methods.iter().map(|m| {
            self.cx
                .rust_name(m.def_id)
                .upper_camel_ident()
                .as_syn_ident()
        });

        let user_handler = all_methods.iter().map(|m| {
            let name = self.cx.rust_name(m.def_id).as_syn_ident();
            let args = m.args.iter().map(|a| self.cx.rust_name(a.def_id).as_syn_ident());
            let has_exception = m.exceptions.is_some();
            let method_result_path = self.method_result_path(&service_name, m);

            let exception = if let Some(p) = &m.exceptions {

                let path = self.cx.cur_related_item_path(p.did);
                quote!{ #path }
            } else {
                quote!(::pilota::thrift::DummyError)
            };

            let convert_exceptions =
                m.exceptions
                    .iter()
                    .map(|p| self.cx.expect_item(p.did))
                    .flat_map(|e| {
                        match &*e {
                    rir::Item::Enum(e) => e.variants.iter().map(|v| {
                        let name = self.cx.rust_name(v.did).as_syn_ident();
                        quote! {
                            Err(::volo_thrift::error::UserError::UserException(#exception::#name(err))) => #method_result_path::#name(err)
                        }
                    }).collect::<Vec<_>>(),
                    _ => panic!()
                }
            });


            if has_exception {
                quote! {
                    match self.inner.#name(#(args.#args),*).await {
                        Ok(resp) => #method_result_path::Ok(resp),
                        #(#convert_exceptions,)*
                        Err(::volo_thrift::error::UserError::Other(err)) => return Err(err),
                    }
                }
            } else {
                quote! {
                    match self.inner.#name(#(args.#args),*).await {
                        Ok(resp) => #method_result_path::Ok(resp),
                        Err(err) => return Err(err),
                    }
                }
            }
        });

        let mk_client_name = format_ident!("Mk{}", generic_client_name);

        stream.extend(quote! {
            pub struct #server_name<S> {
                inner: S, // handler
            }

            pub struct #mk_client_name;

            pub type #client_name = #generic_client_name<::volo::service::BoxCloneService<::volo_thrift::context::ClientContext, #req_send_name, ::std::option::Option<#res_name>, ::volo_thrift::Error>>;

            impl<S> ::volo::client::MkClient<::volo_thrift::Client<S>> for #mk_client_name {
                type Target = #generic_client_name<S>;
                fn mk_client(&self, service: ::volo_thrift::Client<S>) -> Self::Target {
                    #generic_client_name(service)
                }
            }

            #[derive(Clone)]
            pub struct #generic_client_name<S>(pub ::volo_thrift::Client<S>);

            pub struct #oneshot_client_name<S>(pub ::volo_thrift::Client<S>);

            impl<S: ::volo::service::Service<::volo_thrift::context::ClientContext, #req_send_name, Response = ::std::option::Option<#res_name>, Error = ::volo_thrift::Error> + Send + Sync + 'static> #generic_client_name<S> {
                pub fn with_callopt<Opt: ::volo::client::Apply<::volo_thrift::context::ClientContext>>(self, opt: Opt) -> #oneshot_client_name<::volo::client::WithOptService<S, Opt>> {
                    #oneshot_client_name(self.0.with_opt(opt))
                }

                #(#client_methods)*
            }

            impl<S: ::volo::client::OneShotService<::volo_thrift::context::ClientContext, #req_send_name, Response = ::std::option::Option<#res_name>, Error = ::volo_thrift::Error> + Send + Sync + 'static> #oneshot_client_name<S> {
                #(#oneshot_client_methods)*
            }

            pub struct #client_builder_name {
            }

            impl #client_builder_name {
                pub fn new(service_name: impl AsRef<str>) -> ::volo_thrift::client::ClientBuilder<
                    ::volo::layer::Identity,
                    ::volo::layer::Identity,
                    #mk_client_name,
                    #req_send_name,
                    #res_name,
                    ::volo::net::dial::DefaultMakeTransport,
                    ::volo_thrift::codec::default::DefaultMakeCodec<::volo_thrift::codec::default::ttheader::MakeTTHeaderCodec<::volo_thrift::codec::default::framed::MakeFramedCodec<::volo_thrift::codec::default::thrift::MakeThriftCodec>>>,
                    ::volo::loadbalance::LbConfig<::volo::loadbalance::random::WeightedRandomBalance<()>, ::volo::discovery::DummyDiscover>,
                >
                {
                    ::volo_thrift::client::ClientBuilder::new(service_name, #mk_client_name)
                }
            }


            impl<S> #server_name<S> where S: #service_name + ::core::marker::Send + ::core::marker::Sync + 'static {
                pub fn new(inner: S) -> ::volo_thrift::server::Server<Self, ::volo::layer::Identity, #req_recv_name, ::volo_thrift::codec::default::DefaultMakeCodec<::volo_thrift::codec::default::ttheader::MakeTTHeaderCodec<::volo_thrift::codec::default::framed::MakeFramedCodec<::volo_thrift::codec::default::thrift::MakeThriftCodec>>>> {
                    ::volo_thrift::server::Server::new(Self {
                        inner,
                    })
                }
            }

            impl<T> ::volo::service::Service<::volo_thrift::context::ServerContext, #req_recv_name> for #server_name<T> where T: #service_name + Send + Sync + 'static {
                type Response = #res_name;
                type Error = ::anyhow::Error;

                type Future<'cx> = impl ::std::future::Future<Output = ::std::result::Result<Self::Response, Self::Error>> + 'cx;

                fn call<'cx, 's>(&'s self, _cx: &'cx mut ::volo_thrift::context::ServerContext, req: #req_recv_name) -> Self::Future<'cx> where 's:'cx {
                    async move {
                        match req {
                            #(#req_recv_name::#variants(args) => Ok(
                                #res_name::#variants(
                                    #user_handler
                                )
                            ),)*
                        }
                    }
                }
            }
        });
        self.codegen_service_anonymous_type(stream, def_id);
    }

    fn codegen_service_method(&self, _service_def_id: DefId, method: &Method) -> TokenStream {
        let name = self.cx.rust_name(method.def_id).as_syn_ident();
        let ret_ty = self.inner.codegen_item_ty(method.ret.kind.clone());
        let args = method.args.iter().map(|a| {
            let ty = self.inner.codegen_item_ty(a.ty.kind.clone());
            let ident = self.cx.rust_name(a.def_id).as_syn_ident();
            quote! {
                #ident: #ty
            }
        });

        let exception = if let Some(p) = &method.exceptions {
            let exception = self.inner.cur_related_item_path(p.did);
            quote! { ::volo_thrift::error::UserError<#exception> }
        } else {
            quote!(::volo_thrift::AnyhowError)
        };

        quote::quote! {
            async fn #name(&self, #(#args),*) -> ::core::result::Result<#ret_ty, #exception>;
        }
    }

    fn codegen_enum_impl(&self, def_id: DefId, stream: &mut TokenStream, e: &rir::Enum) {
        self.inner.codegen_enum_impl(def_id, stream, e)
    }

    fn codegen_newtype_impl(&self, def_id: DefId, stream: &mut TokenStream, t: &rir::NewType) {
        self.inner.codegen_newtype_impl(def_id, stream, t)
    }
}

pub struct MkThriftBackend;

impl pilota_build::MakeBackend for MkThriftBackend {
    type Target = VoloThriftBackend;

    fn make_backend(self, context: Arc<Context>) -> Self::Target {
        VoloThriftBackend {
            cx: context.clone(),
            inner: ThriftBackend::new(context),
        }
    }
}
