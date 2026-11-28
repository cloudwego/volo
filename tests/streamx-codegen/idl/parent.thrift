namespace go streamx.parent
namespace rs parent
struct Record { 1: required string text }
exception Failure { 1: required string message }
service ParentService {
    Record Lookup(1: Record request) throws(1: Failure failure),
    Record Scan(1: Record request) (streaming.mode="server"),
} (pilota.rust_wrapper_arc="true")
