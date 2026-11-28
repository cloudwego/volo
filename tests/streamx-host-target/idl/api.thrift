include "parent.thrift"
namespace go boundary.api
namespace rs boundary_thrift
service Echo extends parent.Parent {
    parent.Value Watch(1: parent.Value request) (streaming.mode="server")
}
