include "common.thrift"

namespace rs article.image.cdn

struct CDN {
    1: required i64 id,
    2: required string url,
    3: required common.CommonData common_data,
}