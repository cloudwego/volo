namespace go streamx.shadow
namespace rs shadow
struct Value { 1: required string text }
service Echo {
    Value Other(1: Value request),
    oneway void Notify(1: Value request),
}
