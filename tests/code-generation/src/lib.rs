mod r#gen {
    include!(concat!(env!("OUT_DIR"), "/thrift_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/proto_service_a_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/proto_service_b_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/proto_service_c_gen.rs"));
}

pub use r#gen::*;
pub use proto_service_a_gen::*;
pub use proto_service_b_gen::*;
pub use proto_service_c_gen::*;

#[cfg(test)]
mod tests {
    use pilota::{LinkedBytes, pb::Message};

    use super::*;
    use crate::service_a::full_request::Nested;
    #[test]
    fn test_protobuf_service_a_request_creation() {
        let req = service_a::FullRequest {
            request_id: "test-123".into(),
            user_id: "user-456".into(),
            field_a: "value_a".into(),
            field_b: "value_b".into(),
            field_c: "value_c".into(),
            timestamp: 1234567890,
            ..Default::default()
        };

        assert_eq!(req.request_id, "test-123");
        assert_eq!(req.field_a, "value_a");
        assert_eq!(req.field_b, "value_b");
        assert_eq!(req.field_c, "value_c");
    }

    #[test]
    fn test_protobuf_service_b_request_creation() {
        let req = service_b::PartialRequest {
            request_id: "test-123".into(),
            user_id: "user-456".into(),
            field_b: "value_b".into(),
            timestamp: 1234567890,
            ..Default::default()
        };

        assert_eq!(req.request_id, "test-123");
        assert_eq!(req.field_b, "value_b");
    }

    #[test]
    fn test_protobuf_service_c_request_creation() {
        let req = service_c::MinimalRequest {
            request_id: "test-123".into(),
            user_id: "user-456".into(),
            field_c: "value_c".into(),
            timestamp: 1234567890,
            ..Default::default()
        };

        assert_eq!(req.request_id, "test-123");
        assert_eq!(req.field_c, "value_c");
    }

    // A -> B -> C
    #[test]
    fn test_unknown_fields_preservation() {
        let full_request = service_a::FullRequest {
            request_id: "req-123".into(),
            user_id: "user-456".into(),
            field_a: "data_for_a".into(),
            field_b: "data_for_b".into(),
            field_c: "data_for_c".into(),
            timestamp: 1234567890,
            ..Default::default()
        };

        let mut buf = LinkedBytes::new();
        full_request.encode(&mut buf).expect("encode failed");

        let partial_request =
            service_b::PartialRequest::decode(buf.concat().freeze()).expect("decode failed");

        assert_eq!(partial_request.request_id, "req-123");
        assert_eq!(partial_request.user_id, "user-456");
        assert_eq!(partial_request.field_b, "data_for_b");
        assert_eq!(partial_request.timestamp, 1234567890);

        let mut buf = LinkedBytes::new();
        partial_request.encode(&mut buf).expect("re-encode failed");

        let minimal_request = service_c::MinimalRequest::decode(buf.concat().freeze())
            .expect("decode to service C failed");

        println!("Minimal Request: {:?}", minimal_request);

        assert_eq!(minimal_request.request_id, "req-123");
        assert_eq!(minimal_request.user_id, "user-456");
        assert_eq!(minimal_request.field_c, "data_for_c");
        assert_eq!(minimal_request.timestamp, 1234567890);
    }

    // A -> B -> C -> B -> A
    #[test]
    fn test_unknown_fields_round_trip() {
        let original_request = service_a::FullRequest {
            request_id: "round-trip-123".into(),
            user_id: "user-789".into(),
            field_a: "original_a".into(),
            field_b: "original_b".into(),
            field_c: "original_c".into(),
            timestamp: 9876543210,
            ..Default::default()
        };

        let mut buf = LinkedBytes::new();
        original_request.encode(&mut buf).unwrap();

        let partial = service_b::PartialRequest::decode(buf.concat().freeze()).unwrap();
        assert_eq!(partial.field_b, "original_b");

        let mut buf_to_c = LinkedBytes::new();
        partial.encode(&mut buf_to_c).unwrap();

        let minimal = service_c::MinimalRequest::decode(buf_to_c.concat().freeze()).unwrap();
        assert_eq!(minimal.field_c, "original_c");

        let c_response = service_c::MinimalResponse {
            request_id: minimal.request_id.clone(),
            result: "success".into(),
            response_c: "response_from_c".into(),
            ..Default::default()
        };

        let mut resp_buf = LinkedBytes::new();
        c_response.encode(&mut resp_buf).unwrap();

        let b_response = service_b::PartialResponse::decode(resp_buf.concat().freeze()).unwrap();
        assert_eq!(b_response.result, "success");

        let b_full_response = service_b::PartialResponse {
            response_b: "response_from_b".into(),
            ..b_response
        };

        let mut resp_buf = LinkedBytes::new();
        b_full_response.encode(&mut resp_buf).unwrap();

        let final_response = service_a::FullResponse::decode(resp_buf.concat().freeze()).unwrap();
        assert_eq!(final_response.result, "success");
        assert_eq!(final_response.response_b, "response_from_b");
        assert_eq!(final_response.response_c, "response_from_c");
    }

    #[test]
    fn test_service_a_to_service_b() {
        let a_request = service_a::FullRequest {
            request_id: "test-001".into(),
            user_id: "user-001".into(),
            field_a: "for_a".into(),
            field_b: "for_b".into(),
            field_c: "for_c".into(),
            timestamp: 1000,
            ..Default::default()
        };

        let mut buf = LinkedBytes::new();
        a_request.encode(&mut buf).unwrap();

        let b_request = service_b::PartialRequest::decode(buf.concat().freeze()).unwrap();

        assert_eq!(b_request.request_id, "test-001");
        assert_eq!(b_request.field_b, "for_b");

        let mut buf = LinkedBytes::new();
        b_request.encode(&mut buf).unwrap();

        assert!(buf.len() > 0);
    }

    #[test]
    fn test_service_b_to_service_c() {
        let a_request = service_a::FullRequest {
            request_id: "abc-123".into(),
            user_id: "user-abc".into(),
            field_a: "a_value".into(),
            field_b: "b_value".into(),
            field_c: "c_value".into(),
            timestamp: 999,
            ..Default::default()
        };

        let mut buf_from_a = LinkedBytes::new();
        a_request.encode(&mut buf_from_a).unwrap();

        let b_request = service_b::PartialRequest::decode(buf_from_a.concat().freeze()).unwrap();

        let mut buf_to_c = LinkedBytes::new();
        b_request.encode(&mut buf_to_c).unwrap();

        let c_request = service_c::MinimalRequest::decode(buf_to_c.concat().freeze()).unwrap();

        assert_eq!(c_request.field_c, "c_value");
        assert_eq!(c_request.timestamp, 999);
    }

    #[test]
    fn test_response_unknown_fields_preservation() {
        let c_response = service_c::MinimalResponse {
            request_id: "resp-123".into(),
            result: "ok".into(),
            response_c: "from_c".into(),
            ..Default::default()
        };

        let mut buf = LinkedBytes::new();
        c_response.encode(&mut buf).unwrap();

        let b_response = service_b::PartialResponse::decode(buf.concat().freeze()).unwrap();
        assert_eq!(b_response.result, "ok");

        let b_enhanced_response = service_b::PartialResponse {
            response_b: "from_b".into(),
            ..b_response
        };

        let mut buf = LinkedBytes::new();
        b_enhanced_response.encode(&mut buf).unwrap();

        let a_response = service_a::FullResponse::decode(buf.concat().freeze()).unwrap();

        assert_eq!(a_response.result, "ok");
        assert_eq!(a_response.response_b, "from_b");
        assert_eq!(a_response.response_c, "from_c");
    }

    #[test]
    fn test_generated_types_implement_debug() {
        let _ = format!("{:?}", thrift_gen::test::TestRequest::default());
        let _ = format!("{:?}", service_a::FullRequest::default());
        let _ = format!("{:?}", service_b::PartialRequest::default());
        let _ = format!("{:?}", service_c::MinimalRequest::default());
    }

    #[test]
    fn test_generated_types_implement_clone() {
        let req = service_a::FullRequest {
            request_id: "clone-test".into(),
            ..Default::default()
        };
        let cloned = req.clone();
        assert_eq!(req.request_id, cloned.request_id);
    }

    #[test]
    fn test_generated_types_implement_default() {
        let _ = service_a::FullRequest::default();
        let _ = service_b::PartialRequest::default();
        let _ = service_c::MinimalRequest::default();
    }

    #[test]
    fn test_get_descriptor_proto() {
        let desc = service_a::FullRequest::get_descriptor_proto().unwrap();
        assert_eq!(desc.name(), "FullRequest");
        let desc_b = service_b::PartialRequest::get_descriptor_proto().unwrap();
        assert_eq!(desc_b.name(), "PartialRequest");
        let desc_c = service_c::MinimalRequest::get_descriptor_proto().unwrap();
        assert_eq!(desc_c.name(), "MinimalRequest");

        // nested message
        let nested_desc = Nested::get_descriptor_proto().unwrap();
        assert_eq!(nested_desc.name(), "nested");
        // nested oneof
        let nested_oneof_desc =
            service_a::full_request::NestedOneof::get_descriptor_proto().unwrap();
        assert_eq!(nested_oneof_desc.name(), "nested_oneof");
    }
}
