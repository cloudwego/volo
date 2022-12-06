use std::sync::Arc;

use itertools::Itertools;
use pilota_build::{
    db::RirDatabase,
    rir,
    rir::Method,
    tags::protobuf::{ClientStreaming, ServerStreaming},
    CodegenBackend, Context, DefId, IdentName,
};
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

pub struct MkGrpcBackend;

impl pilota_build::MakeBackend for MkGrpcBackend {
    type Target = VoloGrpcBackend;

    fn make_backend(self, context: Arc<Context>) -> Self::Target {
        VoloGrpcBackend { cx: context }
    }
}

pub struct VoloGrpcBackend {
    cx: Arc<Context>,
}

impl VoloGrpcBackend {
    fn trait_input_ty(&self, ty: pilota_build::ty::Ty, streaming: bool) -> TokenStream {
        let ty = self.cx.codegen_item_ty(ty.kind);

        if streaming {
            quote!(::volo_grpc::Request<::volo_grpc::RecvStream<#ty>>)
        } else {
            quote!(::volo_grpc::Request<#ty>)
        }
    }

    fn trait_output_ty(&self, ty: pilota_build::ty::Ty, streaming: bool) -> TokenStream {
        let ret_ty = self.cx.codegen_item_ty(ty.kind);

        if streaming {
            quote!(::volo_grpc::Response<::volo_grpc::BoxStream<'static, ::std::result::Result<#ret_ty, ::volo_grpc::Status>>>, ::volo_grpc::Status)
        } else {
            quote!(::volo_grpc::Response<#ret_ty>, ::volo_grpc::Status)
        }
    }

    fn client_input_ty(&self, ty: pilota_build::ty::Ty, streaming: bool) -> TokenStream {
        let ty = self.cx.codegen_item_ty(ty.kind);

        if streaming {
            quote!(impl ::volo_grpc::IntoStreamingRequest<Message = #ty>)
        } else {
            quote!(impl ::volo_grpc::IntoRequest<#ty>)
        }
    }

    fn client_output_ty(&self, ty: pilota_build::ty::Ty, streaming: bool) -> TokenStream {
        let ret_ty = self.cx.codegen_item_ty(ty.kind);

        if streaming {
            quote!(::std::result::Result<::volo_grpc::Response<impl ::futures::Stream<Item = ::std::result::Result<#ret_ty,::volo_grpc::Status>>>, ::volo_grpc::Status>)
        } else {
            quote!(::std::result::Result<::volo_grpc::Response<#ret_ty>, ::volo_grpc::Status>)
        }
    }

    fn build_client_req(&self, _ty: pilota_build::ty::Ty, streaming: bool) -> TokenStream {
        if streaming {
            quote!(requests
                .into_streaming_request()
                .map(|s| ::volo_grpc::codegen::StreamExt::map(s, |m| ::std::result::Result::Ok(m))))
        } else {
            quote!(requests.into_request().map(|m| ::futures::stream::once(
                ::futures::future::ready(::std::result::Result::Ok(m))
            )))
        }
    }

    fn build_client_resp(
        &self,
        resp_enum_name: &Ident,
        variant_name: &Ident,
        _ty: pilota_build::ty::Ty,
        streaming: bool,
    ) -> TokenStream {
        let resp_stream = quote! {
            let (mut metadata, extensions, message_stream) = resp.into_parts();
            let mut message_stream = match message_stream {
                #resp_enum_name::#variant_name(stream) => stream,
                #[allow(unreachable_patterns)]
                _ => return Err(::volo_grpc::Status::new(::volo_grpc::Code::Unimplemented, "Method not found.")),
            };
        };

        if streaming {
            quote! {
                #resp_stream
                Ok(::volo_grpc::Response::from_parts(metadata, extensions, message_stream))
            }
        } else {
            quote! {
                #resp_stream
                let message = ::volo_grpc::codegen::StreamExt::try_next(&mut message_stream)
                    .await
                    .map_err(|mut status| {
                        status.metadata_mut().merge(metadata.clone());
                        status
                    })?
                    .ok_or_else(|| ::volo_grpc::Status::new(::volo_grpc::Code::Internal, "Missing response message."))?;
                if let Some(trailers) = message_stream.trailers().await? {
                    metadata.merge(trailers);
                }
                Ok(::volo_grpc::Response::from_parts(metadata, extensions, message))
            }
        }
    }

    fn build_server_req(
        &self,
        req_enum_name: &Ident,
        variant_name: &Ident,
        _ty: pilota_build::ty::Ty,
        streaming: bool,
    ) -> TokenStream {
        let req_stream = quote! {
            let (mut metadata, extensions, message_stream) = req.into_parts();
            let mut message_stream = match message_stream {
                #req_enum_name::#variant_name(stream) => stream,
                #[allow(unreachable_patterns)]
                _ => return Err(::volo_grpc::Status::new(::volo_grpc::Code::Unimplemented, "Method not found.")),
            };
        };
        if streaming {
            quote! {
                #req_stream
                let req = ::volo_grpc::Request::from_parts(metadata, extensions, message_stream);
            }
        } else {
            quote! {
                #req_stream
                ::futures::pin_mut!(message_stream);
                let message = ::volo_grpc::codegen::StreamExt::try_next(&mut message_stream)
                    .await?
                    .ok_or_else(|| ::volo_grpc::Status::new(::volo_grpc::Code::Internal, "Missing request message."))?;
                if let Some(trailers) = message_stream.trailers().await? {
                    metadata.merge(trailers);
                }
                let req = ::volo_grpc::Request::from_parts(metadata, extensions, message);
            }
        }
    }

    fn build_server_call(&self, method: &Method) -> TokenStream {
        let method_name = self.cx.rust_name(method.def_id).as_syn_ident();
        quote! {
            let resp = inner.#method_name(req).await;
        }
    }

    fn build_server_resp(
        &self,
        resp_enum_name: &Ident,
        variant_name: &Ident,
        _ty: pilota_build::ty::Ty,
        streaming: bool,
    ) -> TokenStream {
        if streaming {
            quote!(resp.map(|r| r.map(|s|  #resp_enum_name::#variant_name(s))))
        } else {
            quote!(resp.map(|r| r.map(|m| #resp_enum_name::#variant_name(::std::boxed::Box::pin( ::futures::stream::once(::futures::future::ok(m)))))))
        }
    }
}

impl CodegenBackend for VoloGrpcBackend {
    fn codegen_service_impl(&self, def_id: DefId, stream: &mut TokenStream, s: &rir::Service) {
        let service_name = self.cx.rust_name(def_id).as_syn_ident();
        let server_name = format_ident!("{}Server", service_name);
        let client_builder_name = format_ident!("{}ClientBuilder", service_name);
        let generic_client_name = format_ident!("{}GenericClient", service_name);
        let client_name = format_ident!("{}Client", service_name);
        let oneshot_client_name = format_ident!("{}OneShotClient", service_name);

        let file_id = self.cx.node(def_id).unwrap().file_id;
        let file = self.cx.file(file_id).unwrap();

        let package = file.package.iter().join(".");

        let req_enum_name_send = format_ident!("{}RequestSend", service_name);
        let resp_enum_name_send = format_ident!("{}ResponseSend", service_name);
        let req_enum_name_recv = format_ident!("{}RequestRecv", service_name);
        let resp_enum_name_recv = format_ident!("{}ResponseRecv", service_name);
        let paths = s
            .methods
            .iter()
            .map(|method| format!("/{}.{}/{}", package, s.name, method.name))
            .collect::<Vec<_>>();

        let req_matches = s.methods.iter().map(|method| {
            let variant_name = self
                .cx
                .rust_name(method.def_id)
                .upper_camel_ident()
                .as_syn_ident();
            let path = format!("/{}.{}/{}", package, s.name, method.name);
            let client_streaming = self.cx.node_contains_tag::<ClientStreaming>(method.def_id);
            let input_ty = &method.args[0].ty;

            let server_streaming = self.cx.node_contains_tag::<ServerStreaming>(method.def_id);
            let output_ty = &method.ret;

            let req = self.build_server_req(
                &req_enum_name_recv,
                &variant_name,
                input_ty.clone(),
                client_streaming,
            );

            let call = self.build_server_call(method);

            let resp = self.build_server_resp(
                &resp_enum_name_send,
                &variant_name,
                output_ty.clone(),
                server_streaming,
            );

            quote! {
                #path => {
                    #req
                    #call
                    #resp
                },
            }
        });

        let enum_variant_names = s
            .methods
            .iter()
            .map(|method| {
                self.cx
                    .rust_name(method.def_id)
                    .upper_camel_ident()
                    .as_syn_ident()
            })
            .collect::<Vec<_>>();

        let req_tys = s
            .methods
            .iter()
            .map(|method| self.cx.codegen_item_ty(method.args[0].ty.kind.clone()))
            .collect::<Vec<_>>();
        let resp_tys = s
            .methods
            .iter()
            .map(|method| self.cx.codegen_item_ty(method.ret.kind.clone()))
            .collect::<Vec<_>>();

        let mut client_methods = Vec::new();
        let mut oneshot_client_methods = Vec::new();

        s.methods.iter().for_each(|method| {
            let method_name = self.cx.rust_name(method.def_id).as_syn_ident();

            let path = format!("/{}.{}/{}", package, s.name, method.name);
            let input_ty = &method.args[0].ty;
            let client_streaming = self.cx.node_contains_tag::<ClientStreaming>(method.def_id);
            let req_ty = self.client_input_ty(input_ty.clone(), client_streaming);

            let output_ty = &method.ret;
            let server_streaming = self.cx.node_contains_tag::<ServerStreaming>(method.def_id);

            let variant_name = self.cx.rust_name(method.def_id).upper_camel_ident().as_syn_ident();

            let resp_ty = self.client_output_ty(output_ty.clone(), server_streaming);

            let req = self.build_client_req(input_ty.clone(), client_streaming);

            let resp = self.build_client_resp(&resp_enum_name_recv, &variant_name, output_ty.clone(), server_streaming);

            client_methods.push(
                quote! {
                    pub async fn #method_name(
                        &self,
                        requests: #req_ty,
                    ) -> #resp_ty {
                        let req = #req.map(|message| #req_enum_name_send::#variant_name(::std::boxed::Box::pin(message) as _));
                        let mut cx = self.0.make_cx(#path);

                        let resp = ::volo::Service::call(&self.0, &mut cx, req).await?;

                        #resp
                    }
                }
            );

            oneshot_client_methods.push(
                quote! {
                    pub async fn #method_name(
                        self,
                        requests: #req_ty,
                    ) -> #resp_ty {
                        let req = #req.map(|message| #req_enum_name_send::#variant_name(::std::boxed::Box::pin(message) as _));
                        let mut cx = self.0.make_cx(#path);

                        let resp = ::volo::client::OneShotService::call(self.0, &mut cx, req).await?;

                        #resp
                    }
                }
            );
        });

        let mk_client_name = format_ident!("Mk{}", generic_client_name);

        stream.extend(quote! {
            pub enum #req_enum_name_send {
                #(#enum_variant_names(::volo_grpc::BoxStream<'static, ::std::result::Result<#req_tys, ::volo_grpc::Status>>),)*
            }

            impl ::volo_grpc::SendEntryMessage for #req_enum_name_send {
                fn into_body(self,compression_encoding: Option<::volo_grpc::codec::compression::CompressionEncoding>) -> ::volo_grpc::BoxStream<'static, ::std::result::Result<::volo_grpc::codegen::Bytes, ::volo_grpc::Status>> {
                    match self {
                        #(Self::#enum_variant_names(s) => {
                            ::volo_grpc::codec::encode::encode(s,compression_encoding)
                        },)*
                    }
                }
            }

            pub enum #req_enum_name_recv {
                #(#enum_variant_names(::volo_grpc::RecvStream<#req_tys>),)*
            }

            impl ::volo_grpc::RecvEntryMessage for #req_enum_name_recv {
                fn from_body(method: ::std::option::Option<&str>, body: ::volo_grpc::codegen::hyper::Body, kind: ::volo_grpc::codec::decode::Kind,compression_encoding: Option<::volo_grpc::codec::compression::CompressionEncoding>) -> ::std::result::Result<Self, ::volo_grpc::Status> {
                    match method {
                        #(Some(#paths) => {
                            Ok(Self::#enum_variant_names(::volo_grpc::RecvStream::new(body, kind,compression_encoding)))
                        })*
                        _ => Err(::volo_grpc::Status::new(::volo_grpc::Code::Unimplemented, "Method not found.")),
                    }
                }
            }

            pub enum #resp_enum_name_send {
                #(#enum_variant_names(::volo_grpc::BoxStream<'static, ::std::result::Result<#resp_tys, ::volo_grpc::Status>>),)*
            }

            impl ::volo_grpc::SendEntryMessage for #resp_enum_name_send {
                fn into_body(self,compression_encoding: Option<::volo_grpc::codec::compression::CompressionEncoding>) -> ::volo_grpc::BoxStream<'static, ::std::result::Result<::volo_grpc::codegen::Bytes, ::volo_grpc::Status>> {
                    match self {
                        #(Self::#enum_variant_names(s) => {
                            ::volo_grpc::codec::encode::encode(s,compression_encoding)
                        },)*
                    }
                }
            }

            pub enum #resp_enum_name_recv {
                #(#enum_variant_names(::volo_grpc::RecvStream<#resp_tys>),)*
            }

            impl ::volo_grpc::RecvEntryMessage for #resp_enum_name_recv {
                fn from_body(method: ::std::option::Option<&str>, body: ::volo_grpc::codegen::hyper::Body, kind: ::volo_grpc::codec::decode::Kind,compression_encoding: Option<::volo_grpc::codec::compression::CompressionEncoding>) -> ::std::result::Result<Self, ::volo_grpc::Status>
                where
                    Self: ::core::marker::Sized,
                {
                    match method {
                        #(Some(#paths) => {
                            Ok(Self::#enum_variant_names(::volo_grpc::RecvStream::new(body, kind,compression_encoding)))
                        })*
                        _ => Err(::volo_grpc::Status::new(::volo_grpc::Code::Unimplemented, "Method not found.")),
                    }
                }
            }

            pub struct #client_builder_name {}
            impl #client_builder_name {
                pub fn new(
                    service_name: impl AsRef<str>,
                ) -> ::volo_grpc::client::ClientBuilder<
                    ::volo::layer::Identity,
                    ::volo::layer::Identity,
                    #mk_client_name,
                    ::volo_grpc::layer::loadbalance::LbConfig<::volo::loadbalance::random::WeightedRandomBalance<()>, ::volo::discovery::DummyDiscover>,
                    #req_enum_name_send,
                    #resp_enum_name_recv,
                > {
                    ::volo_grpc::client::ClientBuilder::new(#mk_client_name, service_name)
                }
            }

            pub struct #mk_client_name;

            pub type #client_name = #generic_client_name<::volo::service::BoxCloneService<::volo_grpc::context::ClientContext, ::volo_grpc::Request<#req_enum_name_send>, ::volo_grpc::Response<#resp_enum_name_recv>, ::volo_grpc::Status>>;

            impl<S> ::volo::client::MkClient<::volo_grpc::Client<S>> for #mk_client_name {
                type Target = #generic_client_name<S>;
                fn mk_client(&self, service: ::volo_grpc::Client<S>) -> Self::Target {
                    #generic_client_name(service)
                }
            }

            #[derive(Clone)]
            pub struct #generic_client_name<S>(pub ::volo_grpc::Client<S>);

            pub struct #oneshot_client_name<S>(pub ::volo_grpc::Client<S>);

            impl<S> #generic_client_name<S> where S: ::volo::service::Service<::volo_grpc::context::ClientContext, ::volo_grpc::Request<#req_enum_name_send>, Response=::volo_grpc::Response<#resp_enum_name_recv>, Error = ::volo_grpc::Status> + Sync + Send + 'static {
                pub fn with_callopt<Opt: ::volo::client::Apply<::volo_grpc::context::ClientContext>>(self, opt: Opt) -> #oneshot_client_name<::volo::client::WithOptService<S, Opt>> {
                    #oneshot_client_name(self.0.with_opt(opt))
                }

                #(#client_methods)*
            }

            impl<S: ::volo::client::OneShotService<::volo_grpc::context::ClientContext,::volo_grpc::Request<#req_enum_name_send>, Response=::volo_grpc::Response<#resp_enum_name_recv>, Error = ::volo_grpc::Status> + Send + Sync + 'static> #oneshot_client_name<S> {
                #(#oneshot_client_methods)*
            }

            pub struct #server_name<S> {
                inner: ::std::sync::Arc<S>,
            }

            impl<S> Clone for #server_name<S> {
                fn clone(&self) -> Self {
                    #server_name {
                        inner: self.inner.clone(),
                    }
                }
            }

            impl<S> #server_name<S> {
                pub fn new(inner: S) -> ::volo_grpc::server::Server<Self, ::volo::layer::Identity> {
                    let service = Self {
                        inner: ::std::sync::Arc::new(inner),
                    };
                    ::volo_grpc::server::Server::new(service)
                }
            }

            impl<S> ::volo::service::Service<::volo_grpc::context::ServerContext, ::volo_grpc::Request<#req_enum_name_recv>> for #server_name<S>
            where
                S: #service_name + ::core::marker::Send + ::core::marker::Sync + 'static,
            {
                type Response = ::volo_grpc::Response<#resp_enum_name_send>;
                type Error = ::volo_grpc::status::Status;
                type Future<'cx> = impl ::std::future::Future<Output = ::std::result::Result<Self::Response, Self::Error>> + 'cx;

                fn call<'cx, 's>(&'s self, cx: &'cx mut ::volo_grpc::context::ServerContext, req: ::volo_grpc::Request<#req_enum_name_recv>) -> Self::Future<'cx>
                where
                    's: 'cx,
                {
                    let inner = self.inner.clone();
                    async move {
                        match cx.rpc_info.method().unwrap().as_str() {
                            #(#req_matches)*
                            path => {
                                let path = path.to_string();
                                Err(::volo_grpc::Status::unimplemented(::std::format!("Unimplemented http path: {}", path)))
                            }
                        }
                    }
                }
            }
        });
    }

    fn codegen_service_method(
        &self,
        _service_def_id: DefId,
        method: &rir::Method,
    ) -> proc_macro2::TokenStream {
        let client_streaming = self.cx.node_contains_tag::<ClientStreaming>(method.def_id);
        let args = method.args.iter().map(|a| {
            let ty = self.trait_input_ty(a.ty.clone(), client_streaming);

            let ident = format_ident!("{}", a.name);
            quote::quote! {
                #ident: #ty
            }
        });

        let ret_ty = self.trait_output_ty(
            method.ret.clone(),
            self.cx.node_contains_tag::<ServerStreaming>(method.def_id),
        );

        let name = self.cx.rust_name(method.def_id).as_syn_ident();

        quote::quote! {
            async fn #name(&self, #(#args),*) -> ::std::result::Result<#ret_ty>;
        }
    }
}
