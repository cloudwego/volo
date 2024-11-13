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
