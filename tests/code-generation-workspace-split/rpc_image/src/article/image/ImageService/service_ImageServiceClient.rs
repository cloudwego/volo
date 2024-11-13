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
