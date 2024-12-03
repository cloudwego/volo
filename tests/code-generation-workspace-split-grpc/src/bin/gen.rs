use volo_build::plugin::SerdePlugin;

fn main() {
    volo_build::workspace::Builder::protobuf()
        .plugin(SerdePlugin)
        .gen()
}
