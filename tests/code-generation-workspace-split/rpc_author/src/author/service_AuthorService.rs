
pub trait AuthorService {
    fn get_author(
        &self,
        req: GetAuthorRequest,
    ) -> impl ::std::future::Future<
        Output = ::core::result::Result<GetAuthorResponse, ::volo_thrift::ServerError>,
    > + Send;
}
include!("AuthorService/mod.rs");
