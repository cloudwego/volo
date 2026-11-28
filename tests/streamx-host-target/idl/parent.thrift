namespace go boundary.parent
namespace rs boundary_parent
struct Value { 1: required string text }
service Parent { Value Inherited(1: Value request) }
