include "grpc_parent.thrift"

namespace go grpc.payload.api
namespace rs grpc_thrift

struct Empty {}

service Interop extends grpc_parent.Parent {
    grpc_parent.Message Unary(1: grpc_parent.Message request),
    grpc_parent.Message ExplicitUnary(1: grpc_parent.Message request) (streaming.mode="unary"),
    void Zero(),
    i64 Multi(2: i32 left, 9: i64 right),
    list<i64> Container(7: list<i64> values),
    grpc_parent.Message Throw(1: grpc_parent.Message request)
        throws (2: grpc_parent.Failure failure, 7: grpc_parent.Other other),
    void VoidThrow() throws (4: grpc_parent.Failure failure),
    grpc_parent.Message Client(1: grpc_parent.Message request) (streaming.mode="client"),
    grpc_parent.Message Server(1: grpc_parent.Message request) (streaming.mode="server"),
    grpc_parent.Message Bidi(1: grpc_parent.Message request) (streaming.mode="bidirectional"),
}

service PureUnary { Empty Echo(1: Empty request) }
service PureStreaming { Empty Bidi(1: Empty request) (streaming.mode="bidirectional") }
