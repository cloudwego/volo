namespace rs arc_shapes
namespace go a.b.c
struct Message { 1: string text }
struct S { 1: i32 value }

service ArcShapes {
    i32 Scalar(1: i32 value)
    list<i32> Numbers(1: list<i32> value)
    list<Message> Messages(1: list<Message> value)
    S Shadow(1: S value)
  void Empty()
} (pilota.rust_wrapper_arc="true")
