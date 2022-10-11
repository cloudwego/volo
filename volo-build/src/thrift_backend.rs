use std::sync::Arc;

use pilota_build::{
    codegen::thrift::DecodeHelper, db::RirDatabase, rir, rir::Method, Context, DefId, IdentName,
    ThriftBackend,
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
        let args_names = methods
            .iter()
            .map(|m| self.method_args_path(&service_name, m));

        let result_names = methods
            .iter()
            .map(|m| self.method_result_path(&service_name, m));

        let req_name = format_ident!("{}Request", service_name);
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
                impl ::volo_thrift::EntryMessage for #req_name {
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

                    async fn decode_async<R>(
                        protocol: &mut ::pilota::thrift::TAsyncBinaryProtocol<R>,
                        msg_ident: &::pilota::thrift::TMessageIdentifier
                    ) -> ::core::result::Result<Self, ::volo_thrift::Error>
                    where
                        R: ::pilota::AsyncRead + ::core::marker::Unpin + ::core::marker::Send {
                            #decode_async
                        }

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &T) -> usize {
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

                    async fn decode_async<R>(
                        protocol: &mut ::pilota::thrift::TAsyncBinaryProtocol<R>,
                        msg_ident: &::pilota::thrift::TMessageIdentifier,
                    ) -> ::core::result::Result<Self, ::volo_thrift::Error>
                    where
                        R: ::pilota::AsyncRead + ::core::marker::Unpin + ::core::marker::Send {
                            #decode_async
                        }

                    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, protocol: &T) -> usize {
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
            pub enum #req_name {
                #(#variant_names(#args_names)),*
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
                let ident = format_ident!(
                    "{}{}{}",
                    target_service.name,
                    self.cx
                        .rust_name(method.def_id)
                        .upper_camel_ident()
                        .as_syn_ident(),
                    suffix,
                );
                let mut path: syn::Path = self.cx.cur_related_item_path(def_id);
                path.segments.pop();
                path.segments.push(ident.into());
                quote! { #path }
            }
            rir::MethodSource::Own => {
                let ident = format_ident!(
                    "{}{}{}",
                    service_name,
                    self.cx
                        .rust_name(method.def_id)
                        .upper_camel_ident()
                        .as_syn_ident(),
                    suffix
                );
                quote!(#ident)
            }
        }
    }

    fn method_args_path(&self, service_name: &Ident, method: &Method) -> TokenStream {
        self.method_ty_path(service_name, method, "Args")
    }

    fn method_result_path(&self, service_name: &Ident, method: &Method) -> TokenStream {
        self.method_ty_path(service_name, method, "Result")
    }
}

impl pilota_build::CodegenBackend for VoloThriftBackend {
    fn codegen_struct_impl(
        &self,
        def_id: DefId,
        stream: &mut proc_macro2::TokenStream,
        s: &rir::Message,
    ) {
        self.inner.codegen_struct_impl(def_id, stream, s)
    }

    fn codegen_service_impl(
        &self,
        def_id: DefId,
        stream: &mut proc_macro2::TokenStream,
        _s: &rir::Service,
    ) {
        let service_name = self.cx.rust_name(def_id).as_syn_ident();
        let server_name = format_ident!("{}Server", service_name);
        let client_name = format_ident!("{}Client", service_name);
        let client_builder_name = format_ident!("{}Builder", client_name);
        let req_name = format_ident!("{}Request", service_name);
        let res_name = format_ident!("{}Response", service_name);

        let all_methods = self.cx.service_methods(def_id);

        let client_methods = all_methods.iter().map(|m| {
            let name = self.cx.rust_name(m.def_id).as_syn_ident();
            let resp_type = self.cx.codegen_item_ty(m.ret.kind.clone());
            let req_fields = m.args.iter().map(|a| {
                let name = format_ident!("{}", a.name);
                let ty = self.cx.codegen_item_ty(a.ty.kind.clone());
                quote! {
                    #name: #ty
                }
            });
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
                    None => unreachable!(),
                }
            };
            let req_field_names = m.args.iter().map(|a| format_ident!("{}", a.name));
            let anonymous_args_name = self.method_args_path(&service_name, m);
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
                            #res_name::#enum_variant(#result_path::#name(err)) => Err(::volo_thrift::error::ResponseError::UserException(#exception::#name(err)))
                        }
                    }).collect::<Vec<_>>(),
                    _ => panic!()
                }
            });

            quote! {
                pub async fn #name(&mut self #(, #req_fields)*) -> ::std::result::Result<#resp_type, ::volo_thrift::error::ResponseError<#exception>> {
                    let req = #req_name::#enum_variant(#anonymous_args_name {
                        #(#req_field_names),*
                    });
                    match self.client.as_mut().unwrap().call(#method_name_str, req, #oneway).await? {
                        Some(resp) => match resp {
                            #res_name::#enum_variant(#result_path::Ok(resp)) => Ok(resp),
                            #(#convert_exceptions,)*
                            #[allow(unreachable_patterns)]
                            _ => unreachable!(),
                        }
                        #none
                    }
                }
            }
        });

        let variants = all_methods.iter().map(|m| {
            self.cx
                .rust_name(m.def_id)
                .upper_camel_ident()
                .as_syn_ident()
        });

        let user_handler = all_methods.iter().map(|m| {
            let name = self.cx.rust_name(m.def_id).as_syn_ident();
            let args = m.args.iter().map(|a| format_ident!("{}", a.name));
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
                    match inner.#name(#(args.#args),*).await {
                        Ok(resp) => #method_result_path::Ok(resp),
                        #(#convert_exceptions,)*
                        Err(::volo_thrift::error::UserError::Other(err)) => return Err(err),
                    }
                }
            } else {
                quote! {
                    match inner.#name(#(args.#args),*).await {
                        Ok(resp) => #method_result_path::Ok(resp),
                        Err(err) => return Err(err),
                    }
                }
            }
        });

        stream.extend(quote! {
            pub struct #server_name<S, Req> {
                inner: ::std::sync::Arc<S>, // handler
                _marker: ::core::marker::PhantomData<Req>,
            }

            #[derive(Clone)]
            pub struct #client_name {
                client: Option<::volo_thrift::Client<#req_name, #res_name>>
            }

            impl #client_name {
                pub fn new() -> Self {
                    #client_name { client: None }
                }

                pub fn with_callopt(mut self, callopt: ::volo_thrift::client::CallOpt) -> Self {
                    self.client.as_mut().unwrap().set_callopt(callopt);
                    self
                }

                #(#client_methods)*
            }

            impl ::volo_thrift::client::SetClient<#req_name, #res_name> for #client_name {
                fn set_client(mut self, client: ::volo_thrift::client::Client<#req_name, #res_name>) -> #client_name {
                    #client_name {
                        client: Some(client)
                    }
                }
            }

            pub struct #client_builder_name {
            }

            impl #client_builder_name {
                pub fn new(service_name: impl AsRef<str>) -> ::volo_thrift::client::ClientBuilder<
                    ::volo::layer::Identity,
                    ::volo::layer::Identity,
                    #client_name,
                    #req_name,
                    #res_name,
                    ::volo_thrift::codec::MakeClientEncoder<::volo_thrift::codec::tt_header::DefaultTTHeaderCodec>,
                    ::volo_thrift::codec::MakeClientDecoder<::volo_thrift::codec::tt_header::DefaultTTHeaderCodec>,
                    ::volo::loadbalance::LbConfig<::volo::loadbalance::random::WeightedRandomBalance<()>, ::volo::discovery::DummyDiscover>,
                >
                {
                    ::volo_thrift::client::ClientBuilder::new(service_name, #client_name::new())
                }
            }


            impl<S, Req> Clone for #server_name<S, Req> {
                fn clone(&self) -> Self {
                    Self {
                        inner: self.inner.clone(),
                        _marker: ::core::marker::PhantomData,
                    }
                }
            }

            impl<S> #server_name<S, #req_name> where S: #service_name + ::core::marker::Send + ::core::marker::Sync + 'static {
                pub fn new(inner: S) -> ::volo_thrift::server::Server<Self, ::volo::layer::Identity, #req_name, ::volo_thrift::codec::MakeServerEncoder<::volo_thrift::codec::tt_header::DefaultTTHeaderCodec>, ::volo_thrift::codec::MakeServerDecoder<::volo_thrift::codec::tt_header::DefaultTTHeaderCodec>> {
                    let service = Self {
                        inner: ::std::sync::Arc::new(inner),
                        _marker: ::core::marker::PhantomData,
                    };
                    ::volo_thrift::server::Server::new(service)
                }
            }

            impl<T> ::volo::service::Service<::volo_thrift::context::ServerContext, #req_name> for #server_name<T, #req_name> where T: #service_name + Send + Sync + 'static {
                type Response = #res_name;
                type Error = ::anyhow::Error;

                type Future<'cx> = impl ::std::future::Future<Output = ::std::result::Result<Self::Response, Self::Error>> + 'cx;

                fn call<'cx, 's>(&mut self, _cx: &'cx mut ::volo_thrift::context::ServerContext, req: #req_name) -> Self::Future<'cx> where 's:'cx {
                    let inner = self.inner.clone();
                    async move {
                        let res: ::anyhow::Result<#res_name> = match req {
                            #(#req_name::#variants(args) => Ok(
                                #res_name::#variants(
                                    #user_handler
                                )
                            ),)*
                        };
                        res
                    }
                }

            }
        });
        self.codegen_service_anonymous_type(stream, def_id);
    }

    fn codegen_service_method(
        &self,
        _service_def_id: DefId,
        method: &rir::Method,
    ) -> proc_macro2::TokenStream {
        let name = self.cx.rust_name(method.def_id).as_syn_ident();
        let ret_ty = self.inner.codegen_item_ty(method.ret.kind.clone());
        let args = method.args.iter().map(|a| {
            let ty = self.inner.codegen_item_ty(a.ty.kind.clone());
            let ident = format_ident!("{}", a.name);
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
            async fn #name(&self, #(#args),*) -> ::core::result::Result<#ret_ty,#exception>;
        }
    }

    fn codegen_enum_impl(
        &self,
        def_id: DefId,
        stream: &mut proc_macro2::TokenStream,
        e: &rir::Enum,
    ) {
        self.inner.codegen_enum_impl(def_id, stream, e)
    }

    fn codegen_newtype_impl(
        &self,
        def_id: DefId,
        stream: &mut proc_macro2::TokenStream,
        t: &rir::NewType,
    ) {
        self.inner.codegen_newtype_impl(def_id, stream, t)
    }
}

pub struct MkThriftBackend;

impl pilota_build::MakeBackend for MkThriftBackend {
    type Target = VoloThriftBackend;

    fn make_backend(self, context: std::sync::Arc<pilota_build::Context>) -> Self::Target {
        VoloThriftBackend {
            cx: context.clone(),
            inner: ThriftBackend::new(context),
        }
    }
}
