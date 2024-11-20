include "image.thrift"
include "author.thrift"
include "common.thrift"

namespace rs article

enum Status {
    NORMAL = 0,
    DELETED = 1,
}

struct Article {
    1: required i64 id,
    2: required string title,
    3: required string content,
    4: required author.Author author,
    5: required Status status,
    6: required list<image.Image> images,
    7: required common.CommonData common_data,
}

struct GetArticleRequest {
    1: required i64 id,
}

struct GetArticleResponse {
    1: required Article article,
}

service ArticleService {
    GetArticleResponse GetArticle(1: GetArticleRequest req),
}

service articleService {}