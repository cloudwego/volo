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
