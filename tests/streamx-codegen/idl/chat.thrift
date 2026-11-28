namespace go streamx.chat
namespace rs chat

include "parent.thrift"

struct Message { 1: required string text, 2: required i32 sequence }
exception Missing { 1: required string reason }
exception Denied { 1: required i32 code }

service ChatService {
    Message Get(1: Message request) throws (1: Missing missing, 2: Denied denied),
    void Ping(),
    i64 Add(1: i32 left, 3: i64 right),
    Message Upload(1: Message request) (streaming.mode="client"),
    Message Watch(1: Message request) (streaming.mode="server"),
    Message Talk(1: Message request) (streaming.mode="bidirectional"),
    oneway void Fire(1: Message request),
}
service ArchiveService { Message Get(1: Message request) }
service InheritedService extends parent.ParentService { Message Own(1: Message request) }

service SharedService {
    Message(pilota.rust_wrapper_arc="true") Shared(1: Message request(pilota.rust_wrapper_arc="true")),
    Message(pilota.rust_wrapper_arc="true") SharedUpload(1: Message request(pilota.rust_wrapper_arc="true")) (streaming.mode="client"),
    Message(pilota.rust_wrapper_arc="true") SharedWatch(1: Message request(pilota.rust_wrapper_arc="true")) (streaming.mode="server"),
    Message(pilota.rust_wrapper_arc="true") SharedTalk(1: Message request(pilota.rust_wrapper_arc="true")) (streaming.mode="bidirectional"),
}
