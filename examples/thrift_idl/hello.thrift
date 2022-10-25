namespace rs hello

struct HelloRequest {
    1: required string name,
}

struct HelloResponse {
    1: required string message,
}

service HelloService {
    HelloResponse Hello (1: HelloRequest req),
}
