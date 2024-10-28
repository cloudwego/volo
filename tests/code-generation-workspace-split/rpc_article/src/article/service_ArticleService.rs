
pub trait ArticleService {
    fn get_article(
        &self,
        req: GetArticleRequest,
    ) -> impl ::std::future::Future<
        Output = ::core::result::Result<GetArticleResponse, ::volo_thrift::ServerError>,
    > + Send;
}
pub struct ArticleServiceServer<S> {
    inner: S, // handler
}

pub struct MkArticleServiceGenericClient;

pub type ArticleServiceClient = ArticleServiceGenericClient<
    ::volo::service::BoxCloneService<
        ::volo_thrift::context::ClientContext,
        ArticleServiceRequestSend,
        ::std::option::Option<ArticleServiceResponseRecv>,
        ::volo_thrift::ClientError,
    >,
>;

impl<S> ::volo::client::MkClient<::volo_thrift::Client<S>> for MkArticleServiceGenericClient {
    type Target = ArticleServiceGenericClient<S>;
    fn mk_client(&self, service: ::volo_thrift::Client<S>) -> Self::Target {
        ArticleServiceGenericClient(service)
    }
}

#[derive(Clone)]
pub struct ArticleServiceGenericClient<S>(pub ::volo_thrift::Client<S>);

pub struct ArticleServiceOneShotClient<S>(pub ::volo_thrift::Client<S>);

impl<
        S: ::volo::service::Service<
                ::volo_thrift::context::ClientContext,
                ArticleServiceRequestSend,
                Response = ::std::option::Option<ArticleServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > ArticleServiceGenericClient<S>
{
    pub fn with_callopt<Opt: ::volo::client::Apply<::volo_thrift::context::ClientContext>>(
        self,
        opt: Opt,
    ) -> ArticleServiceOneShotClient<::volo::client::WithOptService<S, Opt>> {
        ArticleServiceOneShotClient(self.0.with_opt(opt))
    }

    pub async fn get_article(
        &self,
        req: GetArticleRequest,
    ) -> ::std::result::Result<GetArticleResponse, ::volo_thrift::ClientError> {
        let req = ArticleServiceRequestSend::GetArticle(ArticleServiceGetArticleArgsSend { req });
        let mut cx = self.0.make_cx("GetArticle", false);
        #[allow(unreachable_patterns)]
        let resp = match ::volo::service::Service::call(&self.0, &mut cx, req).await? {
            Some(ArticleServiceResponseRecv::GetArticle(
                ArticleServiceGetArticleResultRecv::Ok(resp),
            )) => ::std::result::Result::Ok(resp),
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
                ArticleServiceRequestSend,
                Response = ::std::option::Option<ArticleServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > ArticleServiceOneShotClient<S>
{
    pub async fn get_article(
        self,
        req: GetArticleRequest,
    ) -> ::std::result::Result<GetArticleResponse, ::volo_thrift::ClientError> {
        let req = ArticleServiceRequestSend::GetArticle(ArticleServiceGetArticleArgsSend { req });
        let mut cx = self.0.make_cx("GetArticle", false);
        #[allow(unreachable_patterns)]
        let resp = match ::volo::client::OneShotService::call(self.0, &mut cx, req).await? {
            Some(ArticleServiceResponseRecv::GetArticle(
                ArticleServiceGetArticleResultRecv::Ok(resp),
            )) => ::std::result::Result::Ok(resp),
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

pub struct ArticleServiceClientBuilder {}

impl ArticleServiceClientBuilder {
    pub fn new(
        service_name: impl AsRef<str>,
    ) -> ::volo_thrift::client::ClientBuilder<
        ::volo::layer::Identity,
        ::volo::layer::Identity,
        MkArticleServiceGenericClient,
        ArticleServiceRequestSend,
        ArticleServiceResponseRecv,
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
        ::volo_thrift::client::ClientBuilder::new(service_name, MkArticleServiceGenericClient)
    }
}

impl<S> ArticleServiceServer<S>
where
    S: ArticleService + ::core::marker::Send + ::core::marker::Sync + 'static,
{
    pub fn new(
        inner: S,
    ) -> ::volo_thrift::server::Server<
        Self,
        ::volo::layer::Identity,
        ArticleServiceRequestRecv,
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

impl<T> ::volo::service::Service<::volo_thrift::context::ServerContext, ArticleServiceRequestRecv>
    for ArticleServiceServer<T>
where
    T: ArticleService + Send + Sync + 'static,
{
    type Response = ArticleServiceResponseSend;
    type Error = ::volo_thrift::ServerError;

    async fn call<'s, 'cx>(
        &'s self,
        _cx: &'cx mut ::volo_thrift::context::ServerContext,
        req: ArticleServiceRequestRecv,
    ) -> ::std::result::Result<Self::Response, Self::Error> {
        match req {
            ArticleServiceRequestRecv::GetArticle(args) => {
                ::std::result::Result::Ok(ArticleServiceResponseSend::GetArticle(
                    match self.inner.get_article(args.req).await {
                        ::std::result::Result::Ok(resp) => {
                            ArticleServiceGetArticleResultSend::Ok(resp)
                        }
                        ::std::result::Result::Err(err) => return ::std::result::Result::Err(err),
                    },
                ))
            }
        }
    }
}
#[derive(Debug, Clone)]
pub enum ArticleServiceRequestRecv {
    GetArticle(ArticleServiceGetArticleArgsRecv),
}

#[derive(Debug, Clone)]
pub enum ArticleServiceRequestSend {
    GetArticle(ArticleServiceGetArticleArgsSend),
}

#[derive(Debug, Clone)]
pub enum ArticleServiceResponseRecv {
    GetArticle(ArticleServiceGetArticleResultRecv),
}

#[derive(Debug, Clone)]
pub enum ArticleServiceResponseSend {
    GetArticle(ArticleServiceGetArticleResultSend),
}

impl ::volo_thrift::EntryMessage for ArticleServiceRequestRecv {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetArticle(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetArticle" => Self::GetArticle(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetArticle" => Self::GetArticle(
                <ArticleServiceGetArticleArgsRecv as ::pilota::thrift::Message>::decode_async(
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
            Self::GetArticle(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}

impl ::volo_thrift::EntryMessage for ArticleServiceRequestSend {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetArticle(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetArticle" => Self::GetArticle(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetArticle" => Self::GetArticle(
                <ArticleServiceGetArticleArgsSend as ::pilota::thrift::Message>::decode_async(
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
            Self::GetArticle(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}
impl ::volo_thrift::EntryMessage for ArticleServiceResponseRecv {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetArticle(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetArticle" => Self::GetArticle(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetArticle" => Self::GetArticle(
                <ArticleServiceGetArticleResultRecv as ::pilota::thrift::Message>::decode_async(
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
            Self::GetArticle(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}

impl ::volo_thrift::EntryMessage for ArticleServiceResponseSend {
    fn encode<T: ::pilota::thrift::TOutputProtocol>(
        &self,
        __protocol: &mut T,
    ) -> ::core::result::Result<(), ::pilota::thrift::ThriftException> {
        match self {
            Self::GetArticle(value) => {
                ::pilota::thrift::Message::encode(value, __protocol).map_err(|err| err.into())
            }
        }
    }

    fn decode<T: ::pilota::thrift::TInputProtocol>(
        __protocol: &mut T,
        msg_ident: &::pilota::thrift::TMessageIdentifier,
    ) -> ::core::result::Result<Self, ::pilota::thrift::ThriftException> {
        ::std::result::Result::Ok(match &*msg_ident.name {
            "GetArticle" => Self::GetArticle(::pilota::thrift::Message::decode(__protocol)?),
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
            "GetArticle" => Self::GetArticle(
                <ArticleServiceGetArticleResultSend as ::pilota::thrift::Message>::decode_async(
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
            Self::GetArticle(value) => ::volo_thrift::Message::size(value, __protocol),
        }
    }
}
