
pub trait ImageService {
    fn get_image(
        &self,
        req: GetImageRequest,
    ) -> impl ::std::future::Future<
        Output = ::core::result::Result<GetImageResponse, ::volo_thrift::ServerError>,
    > + Send;
}
include!("ImageService/mod.rs");
