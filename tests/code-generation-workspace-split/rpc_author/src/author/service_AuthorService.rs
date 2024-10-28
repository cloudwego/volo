
pub trait AuthorService {
    fn get_author(
        &self,
        req: GetAuthorRequest,
    ) -> impl ::std::future::Future<
        Output = ::core::result::Result<GetAuthorResponse, ::volo_thrift::ServerError>,
    > + Send;
}
pub struct AuthorServiceServer<S> {
    inner: S, // handler
}

pub struct MkAuthorServiceGenericClient;

pub type AuthorServiceClient = AuthorServiceGenericClient<
    ::volo::service::BoxCloneService<
        ::volo_thrift::context::ClientContext,
        AuthorServiceRequestSend,
        ::std::option::Option<AuthorServiceResponseRecv>,
        ::volo_thrift::ClientError,
    >,
>;

impl<S> ::volo::client::MkClient<::volo_thrift::Client<S>> for MkAuthorServiceGenericClient {
    type Target = AuthorServiceGenericClient<S>;
    fn mk_client(&self, service: ::volo_thrift::Client<S>) -> Self::Target {
        AuthorServiceGenericClient(service)
    }
}

#[derive(Clone)]
pub struct AuthorServiceGenericClient<S>(pub ::volo_thrift::Client<S>);

pub struct AuthorServiceOneShotClient<S>(pub ::volo_thrift::Client<S>);

impl<
        S: ::volo::service::Service<
                ::volo_thrift::context::ClientContext,
                AuthorServiceRequestSend,
                Response = ::std::option::Option<AuthorServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > AuthorServiceGenericClient<S>
{
    pub fn with_callopt<Opt: ::volo::client::Apply<::volo_thrift::context::ClientContext>>(
        self,
        opt: Opt,
    ) -> AuthorServiceOneShotClient<::volo::client::WithOptService<S, Opt>> {
        AuthorServiceOneShotClient(self.0.with_opt(opt))
    }

    pub async fn get_author(
        &self,
        req: GetAuthorRequest,
    ) -> ::std::result::Result<GetAuthorResponse, ::volo_thrift::ClientError> {
        let req = AuthorServiceRequestSend::GetAuthor(AuthorServiceGetAuthorArgsSend { req });
        let mut cx = self.0.make_cx("GetAuthor", false);
        #[allow(unreachable_patterns)]
        let resp = match ::volo::service::Service::call(&self.0, &mut cx, req).await? {
            Some(AuthorServiceResponseRecv::GetAuthor(AuthorServiceGetAuthorResultRecv::Ok(
                resp,
            ))) => ::std::result::Result::Ok(resp),
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
                AuthorServiceRequestSend,
                Response = ::std::option::Option<AuthorServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > AuthorServiceOneShotClient<S>
{
    pub async fn get_author(
        self,
        req: GetAuthorRequest,
    ) -> ::std::result::Result<GetAuthorResponse, ::volo_thrift::ClientError> {
        let req = AuthorServiceRequestSend::GetAuthor(AuthorServiceGetAuthorArgsSend { req });
        let mut cx = self.0.make_cx("GetAuthor", false);
        #[allow(unreachable_patterns)]
        let resp = match ::volo::client::OneShotService::call(self.0, &mut cx, req).await? {
            Some(AuthorServiceResponseRecv::GetAuthor(AuthorServiceGetAuthorResultRecv::Ok(
                resp,
            ))) => ::std::result::Result::Ok(resp),
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

pub struct AuthorServiceClientBuilder {}

impl AuthorServiceClientBuilder {
    pub fn new(
        service_name: impl AsRef<str>,
    ) -> ::volo_thrift::client::ClientBuilder<
        ::volo::layer::Identity,
        ::volo::layer::Identity,
        MkAuthorServiceGenericClient,
        AuthorServiceRequestSend,
        AuthorServiceResponseRecv,
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
        ::volo_thrift::client::ClientBuilder::new(service_name, MkAuthorServiceGenericClient)
    }
}

impl<S> AuthorServiceServer<S>
where
    S: AuthorService + ::core::marker::Send + ::core::marker::Sync + 'static,
{
    pub fn new(
        inner: S,
    ) -> ::volo_thrift::server::Server<
        Self,
        ::volo::layer::Identity,
        AuthorServiceRequestRecv,
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

impl<T> ::volo::service::Service<::volo_thrift::context::ServerContext, AuthorServiceRequestRecv>
    for AuthorServiceServer<T>
where
    T: AuthorService + Send + Sync + 'static,
{
    type Response = AuthorServiceResponseSend;
    type Error = ::volo_thrift::ServerError;

    async fn call<'s, 'cx>(
        &'s self,
        _cx: &'cx mut ::volo_thrift::context::ServerContext,
        req: AuthorServiceRequestRecv,
    ) -> ::std::result::Result<Self::Response, Self::Error> {
        match req {
            AuthorServiceRequestRecv::GetAuthor(args) => ::std::result::Result::Ok(
                AuthorServiceResponseSend::GetAuthor(match self.inner.get_author(args.req).await {
                    ::std::result::Result::Ok(resp) => AuthorServiceGetAuthorResultSend::Ok(resp),
                    ::std::result::Result::Err(err) => return ::std::result::Result::Err(err),
                }),
            ),
        }
    }
}
#[derive(Debug, Clone)]
pub enum AuthorServiceRequestRecv {
    GetAuthor(AuthorServiceGetAuthorArgsRecv),
}

#[derive(Debug, Clone)]
pub enum AuthorServiceRequestSend {
    GetAuthor(AuthorServiceGetAuthorArgsSend),
}

#[derive(Debug, Clone)]
pub enum AuthorServiceResponseRecv {
    GetAuthor(AuthorServiceGetAuthorResultRecv),
}

#[derive(Debug, Clone)]
pub enum AuthorServiceResponseSend {
    GetAuthor(AuthorServiceGetAuthorResultSend),
}

impl ::volo_thrift::EntryMessage for AuthorServiceRequestRecv {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetAuthor(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetAuthor" => Self::GetAuthor(
                <AuthorServiceGetAuthorArgsRecv as ::pilota::thrift::Message>::decode_async(
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
            Self::GetAuthor(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}

impl ::volo_thrift::EntryMessage for AuthorServiceRequestSend {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetAuthor(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetAuthor" => Self::GetAuthor(
                <AuthorServiceGetAuthorArgsSend as ::pilota::thrift::Message>::decode_async(
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
            Self::GetAuthor(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}
impl ::volo_thrift::EntryMessage for AuthorServiceResponseRecv {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetAuthor(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetAuthor" => Self::GetAuthor(
                <AuthorServiceGetAuthorResultRecv as ::pilota::thrift::Message>::decode_async(
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
            Self::GetAuthor(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}

impl ::volo_thrift::EntryMessage for AuthorServiceResponseSend {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetAuthor(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetAuthor" => Self::GetAuthor(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetAuthor" => Self::GetAuthor(
                <AuthorServiceGetAuthorResultSend as ::pilota::thrift::Message>::decode_async(
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
            Self::GetAuthor(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}
