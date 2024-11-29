include "common.thrift"
include "cdn.thrift"

namespace rs article.image

struct Image {
    1: required i64 id,
    2: required string url,
    3: required cdn.CDN cdn,
    4: required common.CommonData common_data,
}

struct GetImageRequest {
    1: required i64 id,
}

struct GetImageResponse {
    1: required Image image,
}

service ImageService {
    GetImageResponse GetImage(1: GetImageRequest req),
}