namespace go grpc.parent
namespace rs grpc_parent

struct Message {
    1: binary payload,
    2: optional i64 sequence,
}

exception Failure { 1: string message }
exception Other { 1: i32 code }

service Parent {
    Message Inherited(3: Message request) (streaming.mode="server"),
}
