namespace go grpc.shapes
namespace rs grpc_shapes

enum Flag { Ready = 1 }
union Choice { 1: string text, 2: binary bytes }
typedef Choice Alias
struct Node { 1: optional Node next, 2: Flag flag }

service Shapes {
    Alias Watch(1: Alias event) (streaming.mode="server", pilota.name="watch_events"),
    Node Unary(1: Node node),
    i32 Scalar(1: i32 value),
    list<i64> Values(1: list<i64> values),
    void Empty(),
    void Match(),
} (pilota.rust_wrapper_arc="true")

struct Box {}
struct Send {}
struct Sync {}
struct Clone {}
struct Option {}
struct Result {}
struct Default {}
struct S {}
struct T { 1: optional T next }

service Shadows {
    S SCall(1: S value),
    T TCall(1: T value),
    Box Call(1: Box value),
    Box Watch(1: Box value) (streaming.mode="server"),
}
