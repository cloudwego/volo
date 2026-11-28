struct Message { 1: required string text }
service Global {
    Message Unary(1: Message request)
}
