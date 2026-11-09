namespace rs audit_parent
struct Request { 1: required i32 value, 2: optional Request nested }
typedef Request Alias
struct Response { 1: required i32 value }
exception Failure { 1: required string message }
service Parent {
 Response Plain(1:Request request) throws(1:Failure failure)
 Response Unary(1:Request request) (streaming.mode="unary")
 Response Upload(1:Alias request) (streaming.mode="client")
 Response Watch(1:Request request) (streaming.mode="server")
} (pilota.rust_wrapper_arc="true")
