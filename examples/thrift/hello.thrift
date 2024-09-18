namespace rs hello
include "common.thrift"
include "common2.thrift"

struct HelloRequest {
    1: required string name,
    254: optional common.CommonReq common,
    255: optional common2.CommonReq common2,
}

struct HelloResponse {
    1: required string message,
}

service HelloService {
    HelloResponse Hello (1: HelloRequest req),
    HelloResponse Hello2 (1: HelloRequest type),
    HelloResponse Hello3 (1: HelloRequest self),
}
