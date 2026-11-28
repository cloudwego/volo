namespace rs interop
struct Message { 1: required string value, 2: required i32 seq }
service Interop {
 Message Unary(1: Message request)
 Message ExplicitUnary(1: Message request) (streaming.mode="unary")
 Message Client(1: Message request) (streaming.mode="client")
 Message Server(1: Message request) (streaming.mode="server")
 Message Bidi(1: Message request) (streaming.mode="bidirectional")
}
service Pure { Message Chat(1: Message request) (streaming.mode="bidirectional") }
