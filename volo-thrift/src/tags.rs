volo::new_type! {
    // transport type in info
    #[derive(Debug, Copy, Clone)]
    pub struct TransportType(&'static str);
}

impl TransportType {
    pub const TRANSPORT_FRAMED: TransportType = TransportType("framed");
    pub const TRANSPORT_UNFRAMED: TransportType = TransportType("unframed");
}
