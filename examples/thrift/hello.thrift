namespace rs hello

struct HelloRequest {
    1: required string name,
    2: string hello,
}

struct HelloResponse {
    1: required string message,
}

service HelloService {
    HelloResponse Hello (1: HelloRequest req),
}
