use bytes::Bytes;
use volo_thrift::{streaming::StreamConfig, transport::ttstream::frame::Frame};

#[test]
fn decode_go_frame_golden_and_reencode_semantics() {
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/ttstream-frames.json")).unwrap();
    let config = StreamConfig::default();
    for case in golden["cases"].as_array().unwrap() {
        let bytes = Bytes::from(hex::decode(case["hex"].as_str().unwrap()).unwrap());
        let frame = Frame::decode(bytes, &config).unwrap();
        let spec = &case["spec"];
        assert_eq!(i64::from(frame.stream_id), spec["sid"].as_i64().unwrap());
        assert_eq!(frame.kind as u64, spec["kind"].as_u64().unwrap());
        assert_eq!(
            &frame.method[..],
            spec["method"].as_str().unwrap().as_bytes()
        );
        assert_eq!(
            hex::encode(&frame.payload),
            spec["payload_hex"].as_str().unwrap()
        );
        for (key, value) in spec["metadata"].as_object().into_iter().flatten() {
            assert_eq!(
                frame.metadata.get(key).unwrap().as_ref(),
                value.as_str().unwrap().as_bytes()
            );
        }
        assert_eq!(
            Frame::decode(frame.encode(&config).unwrap(), &config).unwrap(),
            frame
        );
    }
}
