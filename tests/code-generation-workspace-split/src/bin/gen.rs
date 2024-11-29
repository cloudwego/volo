use volo_build::plugin::SerdePlugin;

fn main() {
    volo_build::workspace::Builder::thrift()
        .plugin(SerdePlugin)
        .gen()
}
