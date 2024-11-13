pub struct ArticleServiceServer<S> {
    inner: S, // handler
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
