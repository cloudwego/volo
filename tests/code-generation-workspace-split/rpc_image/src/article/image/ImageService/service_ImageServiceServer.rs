pub struct ImageServiceServer<S> {
    inner: S, // handler
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
