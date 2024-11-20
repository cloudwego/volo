pub struct MkarticleServiceGenericClient;

pub type articleServiceClient = articleServiceGenericClient<
    ::volo::service::BoxCloneService<
        ::volo_thrift::context::ClientContext,
        articleServiceRequestSend,
        ::std::option::Option<articleServiceResponseRecv>,
        ::volo_thrift::ClientError,
    >,
>;

impl<S> ::volo::client::MkClient<::volo_thrift::Client<S>> for MkarticleServiceGenericClient {
    type Target = articleServiceGenericClient<S>;
    fn mk_client(&self, service: ::volo_thrift::Client<S>) -> Self::Target {
        articleServiceGenericClient(service)
    }
}

#[derive(Clone)]
pub struct articleServiceGenericClient<S>(pub ::volo_thrift::Client<S>);

pub struct articleServiceOneShotClient<S>(pub ::volo_thrift::Client<S>);

impl<
        S: ::volo::service::Service<
                ::volo_thrift::context::ClientContext,
                articleServiceRequestSend,
                Response = ::std::option::Option<articleServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > articleServiceGenericClient<S>
{
    pub fn with_callopt<Opt: ::volo::client::Apply<::volo_thrift::context::ClientContext>>(
        self,
        opt: Opt,
    ) -> articleServiceOneShotClient<::volo::client::WithOptService<S, Opt>> {
        articleServiceOneShotClient(self.0.with_opt(opt))
    }
}

impl<
        S: ::volo::client::OneShotService<
                ::volo_thrift::context::ClientContext,
                articleServiceRequestSend,
                Response = ::std::option::Option<articleServiceResponseRecv>,
                Error = ::volo_thrift::ClientError,
            > + Send
            + Sync
            + 'static,
    > articleServiceOneShotClient<S>
{
}

pub struct articleServiceClientBuilder {}

impl articleServiceClientBuilder {
    pub fn new(
        service_name: impl AsRef<str>,
    ) -> ::volo_thrift::client::ClientBuilder<
        ::volo::layer::Identity,
        ::volo::layer::Identity,
        MkarticleServiceGenericClient,
        articleServiceRequestSend,
        articleServiceResponseRecv,
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
        ::volo_thrift::client::ClientBuilder::new(service_name, MkarticleServiceGenericClient)
    }
}
