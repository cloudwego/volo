mod r#gen {
    include!(concat!(env!("OUT_DIR"), "/thrift_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/proto_gen.rs"));
    include!(concat!(env!("OUT_DIR"), "/descriptor_gen.rs"));
}

pub use descriptor_gen::descriptor;
pub use r#gen::*;
pub use proto_gen::*;

#[cfg(test)]
mod tests {
    use pilota::{
        LinkedBytes,
        pb::{EncodeLengthContext, Message, descriptor_getter::*},
    };

    use super::*;

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

        let mut ctx = EncodeLengthContext::default();
        let len = full_request.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
        full_request.encode(&mut buf).expect("encode failed");

        let partial_request =
            service_b::PartialRequest::decode(buf.concat().freeze()).expect("decode failed");

        assert_eq!(partial_request.request_id, "req-123");
        assert_eq!(partial_request.user_id, "user-456");
        assert_eq!(partial_request.field_b, "data_for_b");
        assert_eq!(partial_request.timestamp, 1234567890);

        let mut ctx = EncodeLengthContext::default();
        let len = partial_request.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
        partial_request.encode(&mut buf).expect("re-encode failed");

        let minimal_request = service_c::MinimalRequest::decode(buf.concat().freeze())
            .expect("decode to service C failed");

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

        let mut ctx = EncodeLengthContext::default();
        let len = original_request.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
        original_request.encode(&mut buf).unwrap();

        let partial = service_b::PartialRequest::decode(buf.concat().freeze()).unwrap();
        assert_eq!(partial.field_b, "original_b");

        let mut ctx = EncodeLengthContext::default();
        let len = partial.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
        partial.encode(&mut buf).unwrap();

        let minimal = service_c::MinimalRequest::decode(buf.concat().freeze()).unwrap();
        assert_eq!(minimal.field_c, "original_c");

        let c_response = service_c::MinimalResponse {
            request_id: minimal.request_id.clone(),
            result: "success".into(),
            response_c: "response_from_c".into(),
            ..Default::default()
        };

        let mut ctx = EncodeLengthContext::default();
        let len = c_response.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
        c_response.encode(&mut buf).unwrap();

        let b_response = service_b::PartialResponse::decode(buf.concat().freeze()).unwrap();
        assert_eq!(b_response.result, "success");

        let b_full_response = service_b::PartialResponse {
            response_b: "response_from_b".into(),
            ..b_response
        };

        let mut ctx = EncodeLengthContext::default();
        let len = b_full_response.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
        b_full_response.encode(&mut buf).unwrap();

        let final_response = service_a::FullResponse::decode(buf.concat().freeze()).unwrap();
        assert_eq!(final_response.result, "success");
        assert_eq!(final_response.response_b, "response_from_b");
        assert_eq!(final_response.response_c, "response_from_c");
    }

    #[test]
    fn test_response_unknown_fields_preservation() {
        let c_response = service_c::MinimalResponse {
            request_id: "resp-123".into(),
            result: "ok".into(),
            response_c: "from_c".into(),
            ..Default::default()
        };

        let mut ctx = EncodeLengthContext::default();
        let len = c_response.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
        c_response.encode(&mut buf).unwrap();

        let b_response = service_b::PartialResponse::decode(buf.concat().freeze()).unwrap();
        assert_eq!(b_response.result, "ok");

        let b_enhanced_response = service_b::PartialResponse {
            response_b: "from_b".into(),
            ..b_response
        };

        let mut ctx = EncodeLengthContext::default();
        let len = b_enhanced_response.encoded_len(&mut ctx);
        let zero_copy_len = ctx.zero_copy_len;
        let mut buf = LinkedBytes::with_capacity(len - zero_copy_len);
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
        // Test nested message types
        let desc = descriptor::Outer::get_descriptor_proto().unwrap();
        assert_eq!(desc.name(), "Outer");
        let inner = descriptor::outer::Inner::get_descriptor_proto().unwrap();
        assert_eq!(inner.name(), "Inner");
        let nested = descriptor::outer::inner::Nested::get_descriptor_proto().unwrap();
        assert_eq!(nested.name(), "Nested");

        // Test top-level message types
        let outer_alt = descriptor::OuterAlt::get_descriptor_proto().unwrap();
        assert_eq!(outer_alt.name(), "OuterAlt");
        let opt = outer_alt.options.as_ref().unwrap(); // should be deprecated
        assert_eq!(opt.deprecated.unwrap(), true);
        let service_ref = descriptor::ServiceReference::get_descriptor_proto().unwrap();
        assert_eq!(service_ref.name(), "ServiceReference");
        let request = descriptor::Request::get_descriptor_proto().unwrap();
        assert_eq!(request.name(), "Request");
        let response = descriptor::Response::get_descriptor_proto().unwrap();
        assert_eq!(response.name(), "Response");
        let optioned = descriptor::OptionedMessage::get_descriptor_proto().unwrap();
        assert_eq!(optioned.name(), "OptionedMessage");

        // Test enum types
        let global_enum = descriptor::GlobalEnum::get_descriptor_proto().unwrap();
        assert_eq!(global_enum.name(), "GlobalEnum");
        let state_enum = descriptor::outer::inner::State::get_descriptor_proto().unwrap();
        assert_eq!(state_enum.name(), "State");

        // Test map types
        let map = request.get_field_descriptor_proto("by_name").unwrap();
        assert_eq!(map.name(), "by_name");

        // Test service types
        let file = descriptor::file_descriptor_proto_descriptor();
        let service = file.get_service_descriptor_proto("ComplexService").unwrap();
        assert_eq!(service.name(), "ComplexService");
        for method in &service.method {
            match method.name() {
                "UnaryGet" => {
                    assert_eq!(method.input_type, Some(".descriptor.Request".into()));
                    assert_eq!(method.output_type, Some(".descriptor.Response".into()));
                }
                "ServerStream" => {
                    assert_eq!(method.input_type, Some(".descriptor.Request".into()));
                    assert_eq!(method.output_type, Some(".descriptor.Response".into()));
                }
                "ClientStream" => {
                    assert_eq!(method.input_type, Some(".descriptor.Request".into()));
                    assert_eq!(method.output_type, Some(".descriptor.Response".into()));
                }
                _ => panic!("unexpected method name {}", method.name()),
            }
        }
    }
}
