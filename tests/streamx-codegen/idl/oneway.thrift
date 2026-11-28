namespace go streamx.oneway_shapes
namespace rs oneway_shapes

service Shapes {
    oneway void Zero()
    oneway void One(3: i64 value)
    oneway void Many(2: i32 left, 9: i64 right)
    i64 Count()
}
