namespace rs test

include "common.thrift"
include "common2.thrift"

struct TestRequest {
    254: optional common.CommonReq common,
    255: optional common2.CommonReq common2,
}

struct TestResponse {
    1: required string message,
}

service TestService {
    TestResponse Test (1: TestRequest req),
    TestResponse test (1: TestRequest Req),
    TestResponse Test2 (1: TestRequest type),
    TestResponse Test3 (1: TestRequest self),
}

service testService {
    TestResponse Test (1: TestRequest req),
    TestResponse test (1: TestRequest Req),
}
