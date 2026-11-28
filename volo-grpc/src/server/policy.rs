use crate::{Status, body::BoxBody, context::ServerContext, message::PayloadKind};

/// Installed only by an explicitly classified mixed-protocol connection.
#[derive(Clone, Copy, Debug)]
pub(super) struct StrictGrpc;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ResponsePolicy {
    pub payload: PayloadKind,
    pub http_status: Option<http::StatusCode>,
}

#[derive(Clone, Debug)]
pub(super) struct HttpRequestInfo {
    pub method: http::Method,
    pub version: http::Version,
}

pub(super) fn validate_request_info(
    info: Option<&HttpRequestInfo>,
    payload: PayloadKind,
    cx: &mut ServerContext,
) -> Result<(), Status> {
    if payload != PayloadKind::Thrift && cx.extensions.get::<StrictGrpc>().is_none() {
        return Ok(());
    }
    if let Some(info) = info {
        let status = if info.method != http::Method::POST {
            Some(http::StatusCode::METHOD_NOT_ALLOWED)
        } else if info.version != http::Version::HTTP_2 {
            Some(http::StatusCode::HTTP_VERSION_NOT_SUPPORTED)
        } else {
            None
        };
        if let Some(status) = status {
            ResponsePolicy {
                payload,
                http_status: Some(status),
            }
            .install(cx);
            return Err(Status::unimplemented("gRPC requires HTTP/2 POST"));
        }
    }
    Ok(())
}

impl ResponsePolicy {
    pub(super) fn install(self, cx: &mut ServerContext) {
        if let Some(owner) = cx.extensions.get::<super::h2::StreamOwner>() {
            owner.set_response_policy(self);
        }
        cx.extensions.insert(self);
    }

    pub fn apply(self, response: &mut http::Response<BoxBody>) {
        response.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static(self.payload.content_type()),
        );
        if let Some(status) = self.http_status {
            *response.status_mut() = status;
        }
    }
}

pub(super) fn error_response(status: Status, cx: &ServerContext) -> http::Response<BoxBody> {
    error_response_with_stream(
        status,
        cx.extensions
            .get::<ResponsePolicy>()
            .copied()
            .unwrap_or_default(),
        cx.extensions.get::<crate::streaming::StreamContext>(),
    )
}

pub(super) fn error_response_with_stream(
    status: Status,
    policy: ResponsePolicy,
    stream: Option<&crate::streaming::StreamContext>,
) -> http::Response<BoxBody> {
    let mut response = status.to_http();
    if let Some(stream) = stream {
        let mut metadata = stream.response_metadata(false);
        metadata.merge(stream.response_metadata(true));
        let mut headers = metadata.into_headers();
        headers.extend(std::mem::take(response.headers_mut()));
        *response.headers_mut() = headers;
    }
    policy.apply(&mut response);
    response
}

pub(super) fn validate_content_type(
    headers: &http::HeaderMap,
    payload: PayloadKind,
    cx: &mut ServerContext,
) -> Result<(), Status> {
    let strict = cx.extensions.get::<StrictGrpc>().is_some();
    if payload != PayloadKind::Thrift && !strict {
        return Ok(());
    }
    let values = headers.get_all(http::header::CONTENT_TYPE);
    let mut values = values.iter();
    let value = values.next();
    let duplicate = values.next().is_some();
    let media = value.map(|value| {
        value
            .as_bytes()
            .split(|byte| *byte == b';')
            .next()
            .unwrap_or_default()
            .trim_ascii()
    });
    let grpc = !duplicate
        && media.is_some_and(|media| {
            media.eq_ignore_ascii_case(b"application/grpc")
                || media.eq_ignore_ascii_case(b"application/grpc+proto")
                || media.eq_ignore_ascii_case(b"application/grpc+thrift")
        });
    let requested =
        if media.is_some_and(|media| media.eq_ignore_ascii_case(b"application/grpc+thrift")) {
            PayloadKind::Thrift
        } else {
            PayloadKind::Protobuf
        };
    if grpc && requested == payload {
        return Ok(());
    }
    if !grpc {
        ResponsePolicy {
            payload,
            http_status: Some(http::StatusCode::UNSUPPORTED_MEDIA_TYPE),
        }
        .install(cx);
    }
    Err(Status::unimplemented(format!(
        "method requires {}",
        payload.content_type()
    )))
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, StatusCode};

    use super::*;

    #[test]
    fn strict_policy_is_explicit_and_checks_both_payloads() {
        let invalid = [
            vec![],
            vec!["text/plain"],
            vec!["application/grpc+json"],
            vec!["application/grpc+"],
            vec!["application/grpc", "application/grpc"],
        ];
        for values in invalid {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append(http::header::CONTENT_TYPE, HeaderValue::from_static(value));
            }
            assert!(
                validate_content_type(
                    &headers,
                    PayloadKind::Protobuf,
                    &mut ServerContext::default()
                )
                .is_ok()
            );
            for payload in [PayloadKind::Thrift, PayloadKind::Protobuf] {
                let mut cx = ServerContext::default();
                cx.extensions.insert(StrictGrpc);
                assert!(validate_content_type(&headers, payload, &mut cx).is_err());
                assert_eq!(
                    cx.extensions.get::<ResponsePolicy>().unwrap().http_status,
                    Some(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                );
            }
        }
        for media in [
            "application/grpc",
            "application/grpc+proto",
            "application/grpc+thrift; charset=utf-8",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(http::header::CONTENT_TYPE, media.parse().unwrap());
            for payload in [PayloadKind::Thrift, PayloadKind::Protobuf] {
                let mut cx = ServerContext::default();
                cx.extensions.insert(StrictGrpc);
                let result = validate_content_type(&headers, payload, &mut cx);
                assert_eq!(
                    result.is_ok(),
                    media.starts_with("application/grpc+thrift")
                        == (payload == PayloadKind::Thrift)
                );
                assert!(cx.extensions.get::<ResponsePolicy>().is_none());
            }
        }
    }
}
