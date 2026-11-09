namespace go codec
namespace rs codec

struct Nested {
    1: required i32 value
    2: optional string label
}

struct CodecMessage {
    1: i64 integer
    2: bool flag
    3: double real
    4: string text
    5: binary blob
    6: list<i32> numbers
    7: map<string, list<i64>> nested
    8: optional string note
    9: set<i16> ids
    10: optional Nested child
    11: optional list<Nested> children
    12: string default_text = "default"
}

typedef CodecMessage MessageAlias

service CodecService {
    CodecMessage Plain(1: CodecMessage request)
    CodecMessage Unary(1: CodecMessage request) (streaming.mode="unary")
    CodecMessage Client(1: CodecMessage request) (streaming.mode="client")
    CodecMessage Server(1: CodecMessage request) (streaming.mode="server")
    MessageAlias Bidi(1: MessageAlias request) (streaming.mode="bidirectional")
}
