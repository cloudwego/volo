pub struct articleServiceServer<S> {
    inner: S, // handler
}

impl<S> articleServiceServer<S>
where
    S: articleService + ::core::marker::Send + ::core::marker::Sync + 'static,
{
    pub fn new(
        inner: S,
    ) -> ::volo_thrift::server::Server<
        Self,
        ::volo::layer::Identity,
        articleServiceRequestRecv,
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

impl<T> ::volo::service::Service<::volo_thrift::context::ServerContext, articleServiceRequestRecv>
    for articleServiceServer<T>
where
    T: articleService + Send + Sync + 'static,
{
    type Response = articleServiceResponseSend;
    type Error = ::volo_thrift::ServerError;

    async fn call<'s, 'cx>(
        &'s self,
        _cx: &'cx mut ::volo_thrift::context::ServerContext,
        req: articleServiceRequestRecv,
    ) -> ::std::result::Result<Self::Response, Self::Error> {
        match req {}
    }
}
