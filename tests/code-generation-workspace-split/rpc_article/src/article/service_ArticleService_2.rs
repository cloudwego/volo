
pub trait ArticleService {
    fn get_article(
        &self,
        req: GetArticleRequest,
    ) -> impl ::std::future::Future<
        Output = ::core::result::Result<GetArticleResponse, ::volo_thrift::ServerError>,
    > + Send;
}
include!("ArticleService_1/mod.rs");
