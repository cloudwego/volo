namespace go custom.original
namespace rs wire_override
struct Value { 1: required string text }
service Renamed { Value Call(1: Value request) }
