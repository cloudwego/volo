pub struct ServerState(&'static str);

impl ServerState {
    pub const DECODE: &'static str = "volo-server-decode";
    pub const HANDLE: &'static str = "volo-server-handle";
    pub const ENCODE: &'static str = "volo-server-encode";
    pub const SERVE: &'static str = "volo-server-serve";
}

pub struct ServerField(&'static str);

impl ServerField {
    pub const SEND_SIZE: &'static str = "volo-server-send-size";
    pub const RECV_SIZE: &'static str = "volo-server-recv-size";
}
