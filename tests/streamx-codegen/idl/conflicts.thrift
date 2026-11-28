namespace go streamx.conflicts
namespace rs conflicts
struct Value { 1: required string text }
service FirstService {
    Value SharedA(1: Value request),
    Value SharedB(1: Value request),
    Value OnlyA(1: Value request),
}
service SecondService {
    Value SharedA(1: Value request),
    Value SharedB(1: Value request),
    Value OnlyB(1: Value request),
}
service PartialFallback { Value SharedA(1: Value request) }
service FullFallback {
    Value SharedA(1: Value request),
    Value SharedB(1: Value request),
}
