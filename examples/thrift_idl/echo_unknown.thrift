namespace rs echo_unknown 

struct EchoRequest {
    3: required string name,
    6: required EchoUnion echo_union,
}

struct EchoResponse {
    3: required string name,
    6: required EchoUnion echo_union,
}

union EchoUnion {
    2: binary b,
}


service EchoService {
    EchoResponse Hello (1: EchoRequest req),
}
