pub struct AuthorServiceServer<S> {
    inner: S, // handler
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
