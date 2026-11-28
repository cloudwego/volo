include "audit_parent.thrift"
namespace rs audit
struct RESULT { 1: required i32 value }
struct OPTION { 1: required string value }
struct OK { 1: required i32 value }
struct SOME { 1: required i32 value }
struct NONE { 1: required i32 value }
service Child extends audit_parent.Parent {
 RESULT Chat(1: RESULT request) (streaming.mode="bidirectional")
 RESULT WatchResult(1: RESULT request) (streaming.mode="server")
} (pilota.rust_wrapper_arc="true")
