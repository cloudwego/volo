
pub trait ImageService {
    fn get_image(
        &self,
        req: GetImageRequest,
    ) -> impl ::std::future::Future<
        Output = ::core::result::Result<GetImageResponse, ::volo_thrift::ServerError>,
    > + Send;
}
pub struct ImageServiceServer<S> {
    inner: S, // handler
}

pub struct MkImageServiceGenericClient;

pub type ImageServiceClient = ImageServiceGenericClient<
    ::volo::service::BoxCloneService<
        ::volo_thrift::context::ClientContext,
        ImageServiceRequestSend,
        ::std::option::Option<ImageServiceResponseRecv>,
        ::volo_thrift::ClientError,
    >,
>;

impl<S> ::volo::client::MkClient<::volo_thrift::Client<S>> for MkImageServiceGenericClient {
    type Target = ImageServiceGenericClient<S>;
    fn mk_client(&self, service: ::volo_thrift::Client<S>) -> Self::Target {
        ImageServiceGenericClient(service)
    }
}

#[derive(Clone)]
pub struct ImageServiceGenericClient<S>(pub ::volo_thrift::Client<S>);

pub struct ImageServiceOneShotClient<S>(pub ::volo_thrift::Client<S>);

impl<
        S: ::volo::service::Service<
                ::volo_thrift::context::ClientContext,
                ImageServiceRequestSend,
                Response = ::std::option::Option<ImageServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > ImageServiceGenericClient<S>
{
    pub fn with_callopt<Opt: ::volo::client::Apply<::volo_thrift::context::ClientContext>>(
        self,
        opt: Opt,
    ) -> ImageServiceOneShotClient<::volo::client::WithOptService<S, Opt>> {
        ImageServiceOneShotClient(self.0.with_opt(opt))
    }

    pub async fn get_image(
        &self,
        req: GetImageRequest,
    ) -> ::std::result::Result<GetImageResponse, ::volo_thrift::ClientError> {
        let req = ImageServiceRequestSend::GetImage(ImageServiceGetImageArgsSend { req });
        let mut cx = self.0.make_cx("GetImage", false);
        #[allow(unreachable_patterns)]
        let resp = match ::volo::service::Service::call(&self.0, &mut cx, req).await? {
            Some(ImageServiceResponseRecv::GetImage(ImageServiceGetImageResultRecv::Ok(resp))) => {
                ::std::result::Result::Ok(resp)
            }
            None => unreachable!(),
            _ => unreachable!(),
        };
        ::volo_thrift::context::CLIENT_CONTEXT_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() < cache.capacity() {
                cache.push(cx);
            }
        });
        resp
    }
}

impl<
        S: ::volo::client::OneShotService<
                ::volo_thrift::context::ClientContext,
                ImageServiceRequestSend,
                Response = ::std::option::Option<ImageServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > ImageServiceOneShotClient<S>
{
    pub async fn get_image(
        self,
        req: GetImageRequest,
    ) -> ::std::result::Result<GetImageResponse, ::volo_thrift::ClientError> {
        let req = ImageServiceRequestSend::GetImage(ImageServiceGetImageArgsSend { req });
        let mut cx = self.0.make_cx("GetImage", false);
        #[allow(unreachable_patterns)]
        let resp = match ::volo::client::OneShotService::call(self.0, &mut cx, req).await? {
            Some(ImageServiceResponseRecv::GetImage(ImageServiceGetImageResultRecv::Ok(resp))) => {
                ::std::result::Result::Ok(resp)
            }
            None => unreachable!(),
            _ => unreachable!(),
        };
        ::volo_thrift::context::CLIENT_CONTEXT_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() < cache.capacity() {
                cache.push(cx);
            }
        });
        resp
    }
}

pub struct ImageServiceClientBuilder {}

impl ImageServiceClientBuilder {
    pub fn new(
        service_name: impl AsRef<str>,
    ) -> ::volo_thrift::client::ClientBuilder<
        ::volo::layer::Identity,
        ::volo::layer::Identity,
        MkImageServiceGenericClient,
        ImageServiceRequestSend,
        ImageServiceResponseRecv,
        ::volo::net::dial::DefaultMakeTransport,
        ::volo_thrift::codec::default::DefaultMakeCodec<
            ::volo_thrift::codec::default::ttheader::MakeTTHeaderCodec<
                ::volo_thrift::codec::default::framed::MakeFramedCodec<
                    ::volo_thrift::codec::default::thrift::MakeThriftCodec,
                >,
            >,
        >,
        ::volo::loadbalance::LbConfig<
            ::volo::loadbalance::random::WeightedRandomBalance<()>,
            ::volo::discovery::DummyDiscover,
        >,
    > {
        ::volo_thrift::client::ClientBuilder::new(service_name, MkImageServiceGenericClient)
    }
}

impl<S> ImageServiceServer<S>
where
    S: ImageService + ::core::marker::Send + ::core::marker::Sync + 'static,
{
    pub fn new(
        inner: S,
    ) -> ::volo_thrift::server::Server<
        Self,
        ::volo::layer::Identity,
        ImageServiceRequestRecv,
        ::volo_thrift::codec::default::DefaultMakeCodec<
            ::volo_thrift::codec::default::ttheader::MakeTTHeaderCodec<
                ::volo_thrift::codec::default::framed::MakeFramedCodec<
                    ::volo_thrift::codec::default::thrift::MakeThriftCodec,
                >,
            >,
        >,
        ::volo_thrift::tracing::DefaultProvider,
    > {
        ::volo_thrift::server::Server::new(Self { inner })
    }
}

impl<T> ::volo::service::Service<::volo_thrift::context::ServerContext, ImageServiceRequestRecv>
    for ImageServiceServer<T>
where
    T: ImageService + Send + Sync + 'static,
{
    type Response = ImageServiceResponseSend;
    type Error = ::volo_thrift::ServerError;

    async fn call<'s, 'cx>(
        &'s self,
        _cx: &'cx mut ::volo_thrift::context::ServerContext,
        req: ImageServiceRequestRecv,
    ) -> ::std::result::Result<Self::Response, Self::Error> {
        match req {
            ImageServiceRequestRecv::GetImage(args) => ::std::result::Result::Ok(
                ImageServiceResponseSend::GetImage(match self.inner.get_image(args.req).await {
                    ::std::result::Result::Ok(resp) => ImageServiceGetImageResultSend::Ok(resp),
                    ::std::result::Result::Err(err) => return ::std::result::Result::Err(err),
                }),
            ),
        }
    }
}
#[derive(Debug, Clone)]
pub enum ImageServiceRequestRecv {
    GetImage(ImageServiceGetImageArgsRecv),
}

#[derive(Debug, Clone)]
pub enum ImageServiceRequestSend {
    GetImage(ImageServiceGetImageArgsSend),
}

#[derive(Debug, Clone)]
pub enum ImageServiceResponseRecv {
    GetImage(ImageServiceGetImageResultRecv),
}

#[derive(Debug, Clone)]
pub enum ImageServiceResponseSend {
    GetImage(ImageServiceGetImageResultSend),
}

impl ::volo_thrift::EntryMessage for ImageServiceRequestRecv {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetImage(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(::pilota::thrift::Message::decode(__protocol)?),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(
                <ImageServiceGetImageArgsRecv as ::pilota::thrift::Message>::decode_async(
                    __protocol,
                )
                .await?,
            ),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {
        match self {
            Self::GetImage(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}

impl ::volo_thrift::EntryMessage for ImageServiceRequestSend {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetImage(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(::pilota::thrift::Message::decode(__protocol)?),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(
                <ImageServiceGetImageArgsSend as ::pilota::thrift::Message>::decode_async(
                    __protocol,
                )
                .await?,
            ),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {
        match self {
            Self::GetImage(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}
impl ::volo_thrift::EntryMessage for ImageServiceResponseRecv {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetImage(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(::pilota::thrift::Message::decode(__protocol)?),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(
                <ImageServiceGetImageResultRecv as ::pilota::thrift::Message>::decode_async(
                    __protocol,
                )
                .await?,
            ),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {
        match self {
            Self::GetImage(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}

impl ::volo_thrift::EntryMessage for ImageServiceResponseSend {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetImage(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(::pilota::thrift::Message::decode(__protocol)?),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    async fn decode_async<T: ::pilota::thrift::TAsyncInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetImage" => Self::GetImage(
                <ImageServiceGetImageResultSend as ::pilota::thrift::Message>::decode_async(
                    __protocol,
                )
                .await?,
            ),
            _ => {
                return ::std::result::Result::Err(::pilota::thrift::new_application_exception(
                    ::pilota::thrift::ApplicationExceptionKind::UNKNOWN_METHOD,
                    format!("unknown method {}", msg_ident.name),
                ));
            }
        })
    }

    fn size<T: ::pilota::thrift::TLengthProtocol>(&self, __protocol: &mut T) -> usize {
        match self {
            Self::GetImage(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}
