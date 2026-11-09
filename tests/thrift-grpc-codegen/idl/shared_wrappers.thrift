namespace rs shared_wrappers

struct Node { 1: optional Node next }

service Wrapped {
    Node Echo(1: Node req),
} (pilota.rust_wrapper_arc="true")
