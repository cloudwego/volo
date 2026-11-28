namespace go streamx.name_edges.parent
namespace rs name_parent
struct Message { 1: required string text }
exception Failure { 1: string reason }
service Parent {
    Message Inherited(3: Message request) (streaming.mode="server")
    Message Fallible(5: Message request) throws (9: Failure failure)
} (pilota.name="Base")
