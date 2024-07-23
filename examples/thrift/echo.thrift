namespace rs echo 

struct EchoRequest {
    1: required string faststr_with_default = "default faststr",
    2: required string faststr,
    3: required string name,
    4: optional map<string, string> map_with_default = {"default": "map"},
    5: optional map<string, string> map,
    6: required EchoUnion echo_union,
    7: required EchoEnum echo_enum,
}

struct EchoResponse {
    1: required string faststr_with_default = "default faststr",
    2: required string faststr,
    3: required string name,
    4: optional map<string, string> map_with_default = {"default": "map"},
    5: optional map<string, string> map,
    6: required EchoUnion echo_union,
    7: required EchoEnum echo_enum,
}

union EchoUnion {
    1: bool a,
    2: binary b,
}

enum EchoEnum {
    A = 1,
    B = 2,
}

service EchoService {
    EchoResponse Hello (1: EchoRequest req),
}
